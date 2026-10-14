//! Render WebKit's proposed context menu with Vampir, retaining its native
//! actions and completion until selection. Completing early releases WebKit's
//! menu proxy and makes image, media and extension actions silently do nothing.

use crate::icons::Icon;
use block2::{Block, RcBlock};
use objc2::{
    Message, ffi,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSMenu, NSMenuItem, NSUserInterfaceItemIdentification};
use vampir::{MenuItem, NativeMenu};
use objc2_web_kit::WKWebView;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt,
};

/// Runs in every frame and reports the text a context menu would search
/// for. The page's message arrives before WebKit proposes its menu.
pub const SELECTION_SCRIPT: &str = r#"(() => {
  const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc;
  if (!handler) return;
  addEventListener('contextmenu', () => {
    let text = '';
    try {
      const field = document.activeElement;
      if (field && field.type !== 'password' &&
          (field.tagName === 'TEXTAREA' || field.tagName === 'INPUT') &&
          typeof field.selectionStart === 'number') {
        text = field.value.slice(field.selectionStart, field.selectionEnd);
      }
    } catch (_) {}
    if (!text) text = String(getSelection() || '');
    handler.postMessage('context-selection:' + text);
  }, true);
})()"#;

type Handler = Box<dyn Fn(usize)>;
thread_local! {
    static NEXT: Cell<usize> = const { Cell::new(0) };
    static PENDING: RefCell<HashMap<usize, (usize, Request)>> = RefCell::default();
    static HANDLERS: RefCell<HashMap<usize, Handler>> = RefCell::default();
}

pub struct Request {
    menu: NativeMenu,
    pub position: gpui::Point<gpui::Pixels>,
    /// The action of WebKit's search row, and what the browser searches
    /// for in its place.
    search: Option<usize>,
    query: Option<String>,
    completion: Option<RcBlock<dyn Fn(*mut NSMenu)>>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageMenu")
            .field("menu", &self.menu)
            .finish()
    }
}

impl Request {
    /// The menu's rows, for the floating menu to show.
    pub fn items(&self) -> Vec<MenuItem<usize>> {
        self.menu.items().to_vec()
    }

    pub fn is_empty(&self) -> bool {
        self.menu.is_empty()
    }

    /// Has the search row search for `query` with the browser's own engine,
    /// named by `label`. Without a selection WebKit's row is left as it is.
    pub fn search_with(&mut self, label: String, query: &str) {
        let query = query.split_whitespace().collect::<Vec<_>>().join(" ");
        if query.is_empty() {
            return;
        }
        if let Some(index) = self.search
            && let Some(row) = self.menu.row_mut(index)
        {
            row.label = label.into();
            self.query = Some(query);
        }
    }

    /// Runs the chosen row's WebKit action, or for the search row returns
    /// the selected text for the browser to search for itself.
    pub fn finish(mut self, chosen: Option<usize>) -> Option<String> {
        if let Some(query) = self.query.take()
            && chosen.is_some()
            && chosen == self.search
        {
            self.complete();
            return Some(query);
        }
        if let Some(index) = chosen {
            // Dispatch while WebKit's completion still owns the menu proxy.
            self.menu.run(index);
        }
        self.complete();
        None
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

/// WebKit's menu as Vampir rows, its actions kept to run on a choice.
fn adopt(menu: &NSMenu) -> NativeMenu {
    NativeMenu::adopt(menu, |item| icon(item).map(Into::into))
}

/// The action of WebKit's "Search with Google" row, if the menu has one.
fn search_row(menu: &NativeMenu) -> Option<usize> {
    menu.find(|action| action.identifier().as_deref() == Some(SEARCH_IDENTIFIER))
}

const SEARCH_IDENTIFIER: &str = "WKMenuItemIdentifierSearchWeb";

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
        let menu = adopt(menu);
        let search = search_row(&menu);
        let request = Request {
            menu,
            search,
            query: None,
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
    menu.addItem(&row("Reload"));
    let adopted = adopt(&menu);
    assert_eq!(adopted.items().len(), 1);
    assert_eq!(search_row(&adopted), None);
    let calls = Rc::new(Cell::new(0));
    let make = || {
        let calls = calls.clone();
        Request {
            menu: adopt(&NSMenu::new(mtm)),
            position: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            search: None,
            query: None,
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
    let completed = state.clone();
    Request {
        menu: adopt(&action_menu),
        position: gpui::point(gpui::px(0.0), gpui::px(0.0)),
        search: None,
        query: None,
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
    let search_menu = NSMenu::new(mtm);
    search_menu.addItem(&row("Copy"));
    let search_row = row("Search with Google");
    search_row.setIdentifier(Some(&NSString::from_str(SEARCH_IDENTIFIER)));
    search_menu.addItem(&search_row);
    let adopted = adopt(&search_menu);
    let index = self::search_row(&adopted).expect("WebKit's search row is recognised by its identifier");
    let mut request = Request {
        menu: adopted,
        position: gpui::point(gpui::px(0.0), gpui::px(0.0)),
        search: Some(index),
        query: None,
        completion: Some(RcBlock::new(|menu: *mut NSMenu| assert!(menu.is_null()))),
    };
    request.search_with("Search with Kagi".into(), "  \n ");
    assert!(
        matches!(&request.items()[1], MenuItem::Action(action) if action.label == "Search with Google"),
        "Without a selection WebKit's row stays"
    );
    request.search_with("Search with Kagi".into(), " vampire\n  bats ");
    assert!(
        matches!(&request.items()[1], MenuItem::Action(action) if action.label == "Search with Kagi")
    );
    assert_eq!(
        request.finish(Some(index)).as_deref(),
        Some("vampire bats"),
        "The browser searches instead of handing the text to Services"
    );
}
