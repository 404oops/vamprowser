#![allow(dead_code, unused_imports)]
// The file-picker module also mentions site permissions; this target exercises
// only its real MenuEntry type, without loading or mutating a browser profile.
mod settings { pub enum SitePermission { Allow, Block } }
// AppKit objects and WebKit menu completions must be exercised on the main
// thread, so this regression target deliberately uses no Rust test harness.
#[path = "../src/interface/icons.rs"]
mod icons;
#[path = "../src/platform/native.rs"]
mod native;
#[path = "../src/browser/page_menu.rs"]
mod page_menu;

fn main() {
    page_menu::verify_native_menu();
    println!("Native page-menu regressions passed.");
}
