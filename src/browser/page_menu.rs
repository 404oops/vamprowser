//! Render WebKit's proposed context menu with Vampir, retaining its native
//! actions and completion until selection. Completing early releases WebKit's
//! menu proxy and makes image, media and extension actions silently do nothing.

use crate::{icons::Icon, native::MenuEntry};
use block2::{Block, RcBlock};
use objc2::{
    Message, ffi,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSMenu, NSMenuItem, NSUserInterfaceItemIdentification};
use objc2_web_kit::WKWebView;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt,
};

type Handler = Box<dyn Fn(usize)>;
thread_local! {
    static NEXT: Cell<usize> = const { Cell::new(0) };
    static PENDING: RefCell<HashMap<usize, (usize, Request)>> = RefCell::default();
    static HANDLERS: RefCell<HashMap<usize, Handler>> = RefCell::default();
}

struct Action {
    menu: Retained<NSMenu>,
    index: isize,
    // NSMenuItem targets are weak. Keep them alive as long as the popup.
    _target: Option<Retained<AnyObject>>,
}

pub struct Request {
    pub entries: Vec<MenuEntry>,
    pub position: gpui::Point<gpui::Pixels>,
    actions: Vec<Option<Action>>,
    completion: Option<RcBlock<dyn Fn(*mut NSMenu)>>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageMenu")
            .field("rows", &self.actions.len())
            .finish()
    }
}

impl Request {
    pub fn finish(mut self, chosen: Option<usize>) {
        if let Some(action) = chosen
            .and_then(|index| self.actions.get(index))
            .and_then(Option::as_ref)
        {
            // Dispatch while WebKit's completion still owns the menu proxy.
            action.menu.performActionForItemAtIndex(action.index);
        }
        self.complete();
    }

    fn complete(&mut self) {
        if let Some(completion) = self.completion.take() {
            // No AppKit popup: Vampir has already handled or dismissed it.
            completion.call((std::ptr::null_mut(),));
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        self.complete();
    }
}

fn icon(item: &NSMenuItem) -> Option<Icon> {
    // Stable WebKit identifiers rather than localized menu titles.
    let identifier = item.identifier()?.to_string();
    if identifier.contains("Download") {
        Some(Icon::Download)
    } else if identifier.contains("Copy") {
        Some(Icon::Duplicate)
    } else if identifier.contains("Open") {
        Some(Icon::Open)
    } else if identifier.contains("Reload") {
        Some(Icon::Reload)
    } else if identifier.contains("Back") {
        Some(Icon::Back)
    } else if identifier.contains("Forward") {
        Some(Icon::Forward)
    } else {
        None
    }
}

fn convert(menu: &NSMenu, actions: &mut Vec<Option<Action>>) -> Vec<MenuEntry> {
    let mut entries = Vec::new();
    for (index, item) in menu.itemArray().iter().enumerate() {
        // Option-key alternates replace their primary row in AppKit menus;
        // drawn here they would duplicate it.
        if item.isHidden() || item.isAlternate() {
            continue;
        }
        if item.isSeparatorItem() {
            if entries.is_empty() || matches!(entries.last(), Some(MenuEntry::Separator)) {
                continue;
            }
            entries.push(MenuEntry::Separator);
            actions.push(None);
        } else if let Some(submenu) = item.submenu() {
            if !item.isEnabled() {
                entries.push(MenuEntry::Item {
                    label: item.title().to_string(),
                    enabled: false,
                    checked: false,
                    icon: icon(&item),
                });
                actions.push(None);
                continue;
            }
            let mut children = Vec::new();
            let child_entries = convert(&submenu, &mut children);
            if child_entries.is_empty() {
                continue;
            }
            actions.push(None); // Submenus occupy an index, too.
            actions.extend(children);
            entries.push(MenuEntry::Submenu {
                label: item.title().to_string(),
                entries: child_entries,
                icon: icon(&item),
            });
        } else {
            entries.push(MenuEntry::Item {
                label: item.title().to_string(),
                enabled: item.isEnabled(),
                checked: item.state() != 0,
                icon: icon(&item),
            });
            actions.push(Some(Action {
                menu: menu.retain(),
                index: index as isize,
                _target: item.target(),
            }));
        }
    }
    if matches!(entries.last(), Some(MenuEntry::Separator)) {
        entries.pop();
        actions.pop();
    }
    entries
}

unsafe extern "C-unwind" fn proposed_menu(
    _this: &AnyObject,
    _cmd: Sel,
    view: &WKWebView,
    menu: &NSMenu,
    _element: &AnyObject,
    _info: *mut AnyObject,
    completion: &Block<dyn Fn(*mut NSMenu)>,
) {
    let key = view as *const WKWebView as usize;
    HANDLERS.with(|handlers| {
        let handlers = handlers.borrow();
        let Some(handler) = handlers.get(&key) else {
            completion.call((Retained::as_ptr(&menu.retain()) as *mut NSMenu,));
            return;
        };
        let Some(window) = view.window() else {
            completion.call((std::ptr::null_mut(),));
            return;
        };
        let Some(content) = window.contentView() else {
            completion.call((std::ptr::null_mut(),));
            return;
        };
        let location =
            content.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
        let y = if content.isFlipped() {
            location.y
        } else {
            content.bounds().size.height - location.y
        };
        let mut actions = Vec::new();
        let entries = convert(menu, &mut actions);
        let request = Request {
            entries,
            actions,
            position: gpui::point(gpui::px(location.x as f32), gpui::px(y as f32)),
            completion: Some(completion.copy()),
        };
        let token = NEXT.with(|next| {
            let token = next.get() + 1;
            next.set(token);
            token
        });
        PENDING.with(|pending| pending.borrow_mut().insert(token, (key, request)));
        handler(token);
    });
}

pub fn enable(view: &WKWebView, handler: impl Fn(usize) + 'static) {
    HANDLERS.with(|handlers| {
        handlers
            .borrow_mut()
            .insert(view as *const WKWebView as usize, Box::new(handler))
    });
    // WebKit caches optional delegate selectors when the delegate is assigned.
    // Install before assigning it again, including on the very first tab.
    unsafe {
        let Some(delegate) = view.UIDelegate() else {
            return;
        };
        let object = &*(&*delegate as *const _ as *const AnyObject);
        let class = object.class();
        type Proposed = unsafe extern "C-unwind" fn(
            &AnyObject,
            Sel,
            &WKWebView,
            &NSMenu,
            &AnyObject,
            *mut AnyObject,
            &Block<dyn Fn(*mut NSMenu)>,
        );
        let imp = std::mem::transmute::<Proposed, Imp>(proposed_menu);
        ffi::class_addMethod(
            class as *const AnyClass as *mut AnyClass,
            sel!(_webView:getContextMenuFromProposedMenu:forElement:userInfo:completionHandler:),
            imp,
            c"v@:@@@@@?".as_ptr(),
        );
        view.setUIDelegate(Some(&delegate));
    }
}

pub fn take(token: usize) -> Option<Request> {
    PENDING.with(|pending| {
        pending
            .borrow_mut()
            .remove(&token)
            .map(|(_, request)| request)
    })
}

pub fn forget(view: &WKWebView) {
    let key = view as *const WKWebView as usize;
    HANDLERS.with(|handlers| handlers.borrow_mut().remove(&key));
    let tokens = PENDING.with(|pending| {
        pending
            .borrow()
            .iter()
            .filter_map(|(token, (owner, _))| (*owner == key).then_some(*token))
            .collect::<Vec<_>>()
    });
    for token in tokens {
        drop(take(token));
    }
}

#[cfg(test)]
use objc2::MainThreadOnly;
#[cfg(test)]
use objc2_foundation::{NSObject, NSObjectProtocol};

#[cfg(test)]
objc2::define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = std::rc::Rc<Cell<usize>>]
    struct MenuTestTarget;
    unsafe impl NSObjectProtocol for MenuTestTarget {}
    impl MenuTestTarget {
        #[unsafe(method(selected:))]
        fn selected(&self, _sender: &AnyObject) {
            use objc2::DefinedClass;
            assert_eq!(self.ivars().get(), 0, "Native action runs before releasing the proxy");
            self.ivars().set(1);
        }
    }
);

#[cfg(test)]
pub fn verify_native_menu() {
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    use std::rc::Rc;
    let mtm = MainThreadMarker::new().expect("AppKit regressions run on the main thread");
    NSApplication::sharedApplication(mtm);
    let menu = NSMenu::new(mtm);
    let row = |title| unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            &NSString::from_str(""),
        )
    };
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&row("Reload"));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let hidden = row("Hidden service");
    hidden.setHidden(true);
    menu.addItem(&hidden);
    let alternate = row("Reload Without Content Blockers");
    alternate.setAlternate(true);
    menu.addItem(&alternate);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let empty = row("Empty submenu");
    empty.setSubmenu(Some(&NSMenu::new(mtm)));
    menu.addItem(&empty);
    let disabled = row("Unavailable");
    disabled.setEnabled(false);
    menu.addItem(&disabled);
    let submenu = NSMenu::new(mtm);
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    let checked = row("Checked option");
    checked.setState(1);
    submenu.addItem(&checked);
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    let parent = row("Options");
    parent.setSubmenu(Some(&submenu));
    menu.addItem(&parent);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let mut actions = Vec::new();
    let entries = convert(&menu, &mut actions);
    assert_eq!(
        entries.len(),
        4,
        "No leading, trailing, repeated separators or empty submenus"
    );
    assert!(matches!(entries[1], MenuEntry::Separator));
    assert!(matches!(entries[2], MenuEntry::Item { enabled: false, .. }));
    match &entries[3] {
        MenuEntry::Submenu { entries, .. } => {
            assert_eq!(entries.len(), 1);
            assert!(matches!(entries[0], MenuEntry::Item { checked: true, .. }));
        }
        _ => panic!("Expected submenu"),
    }
    assert_eq!(
        actions.len(),
        5,
        "Preorder action positions match Vampir's submenu indexes"
    );
    assert_eq!(actions[0].as_ref().unwrap().index, 1);
    assert_eq!(actions[2].as_ref().unwrap().index, 7);
    assert_eq!(actions[4].as_ref().unwrap().index, 1);
    let calls = Rc::new(Cell::new(0));
    let make = || {
        let calls = calls.clone();
        Request {
            entries: vec![],
            position: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            actions: vec![],
            completion: Some(RcBlock::new(move |menu: *mut NSMenu| {
                assert!(menu.is_null(), "Suppress the AppKit popup");
                calls.set(calls.get() + 1);
            })),
        }
    };
    let request = make();
    assert_eq!(
        calls.get(),
        0,
        "Keep WebKit's action proxy alive while the popup is open"
    );
    request.finish(None);
    assert_eq!(calls.get(), 1, "Dismissal completes once");
    drop(make());
    assert_eq!(calls.get(), 2, "Unconsumed requests also complete once");
    let state = Rc::new(Cell::new(0));
    let target = mtm.alloc::<MenuTestTarget>().set_ivars(state.clone());
    let target: Retained<MenuTestTarget> = unsafe { objc2::msg_send![super(target), init] };
    let action_menu = NSMenu::new(mtm);
    action_menu.setAutoenablesItems(false);
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Copy Image"),
            Some(sel!(selected:)),
            &NSString::from_str(""),
        )
    };
    unsafe {
        item.setTarget(Some(&target));
    }
    action_menu.addItem(&item);
    let mut actions = Vec::new();
    let entries = convert(&action_menu, &mut actions);
    let completed = state.clone();
    Request {
        entries,
        position: gpui::point(gpui::px(0.0), gpui::px(0.0)),
        actions,
        completion: Some(RcBlock::new(move |_: *mut NSMenu| {
            assert_eq!(completed.get(), 1);
            completed.set(2);
        })),
    }
    .finish(Some(0));
    assert_eq!(
        state.get(),
        2,
        "Selection dispatches the original native action and completes once"
    );
}
