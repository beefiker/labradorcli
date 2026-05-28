# APP-4154 Phase 2 — Other ObjC objects checklist

Every ownership-producing message send in a non-ARC `.m` file, plus every Rust-side `msg_send![class!(X), alloc]` retained allocation. Each batch agent fills in the trailing columns, applies the fix, and ticks the row. See `TECH.md` for the decision rule.

## Reproducible greps

```
rg -n '\balloc\]|\bnew\]|\bcopy\]|\bmutableCopy\]' -g '*.m' -g '*.mm'
rg -n 'msg_send!\[class!\([A-Za-z_]+\), alloc\]' -g '*.rs'
```

Ignore (not leaks):
- `[super dealloc]` matches (`crates/labrador_ui/src/platform/mac/objc/menus.m:25`).
- `app/DockTilePlugin/LabradorDockTilePlugin.m` — compiled with `-fobjc-arc`.
- Definitions of trait-style alloc helpers (e.g. `unsafe fn alloc(...) -> id { msg_send![class!(NSAlert), alloc] }` in `crates/labrador_ui/src/platform/mac/app.rs:46` when it's a helper; audit the callers instead).

## Row format

```
- [ ] path:line — function — disposition (released|autoreleased|leaked|stored|?) — thread-origin — hot/cold — strategy — action
```

## Batch 2.A — `sentry-objc`

Files: `app/src/platform/mac/objc/crash_reporting.m`.

- [x] app/src/platform/mac/objc/crash_reporting.m:21 — `setUser` — leaked (`[[SentryUser alloc] init]` never released) — appkit-main (called via `set_optional_user_information` on `AppContext`) — cold (auth login/logout) — explicit-release — added `[user release]` after `[SentrySDK setUser:user]`
- [x] app/src/platform/mac/objc/crash_reporting.m:76 — `recordBreadcrumb` — released (post-#560 `[crumb release]` on line 82) — rust-thread (`forward_breadcrumb`, any Rust thread; caller wraps in `NSAutoreleasePool`) — hot — explicit-release — no-op (already correct)

## Batch 2.B — `app-objc-misc`

Files: `app/src/platform/mac/objc/{app_bundle.m, services.m}`.

`app_bundle.m` has no `alloc]`/`new]`/`copy]`/`mutableCopy]` hits — confirmed via re-grep, N/A (no rows to file).

NB: the `@autoreleasepool { ... }` around this function body drains autoreleased temporaries but does NOT balance `[[X alloc] init]`'s +1 retain. The rows below are retained-and-leaked until the enclosing scope exits; they need `autorelease-helper` (swap to `[NSMutableArray array]` etc.) or `explicit-release`, not `ambient`.

- [x] app/src/platform/mac/objc/services.m:30 — `forFilesFromPasteboard:performAction:` — retained (+1 from alloc/init, `@autoreleasepool` does not drain) and leaked prior to fix — appkit-main (Services dispatch) — cold — autorelease-helper — replaced with `[NSMutableArray array]`
- [x] app/src/platform/mac/objc/services.m:35 — `forFilesFromPasteboard:performAction:` — retained (+1 from alloc/init) and leaked prior to fix — appkit-main — cold — autorelease-helper — replaced with `[NSMutableArray array]`
- [x] app/src/platform/mac/objc/services.m:37 — `forFilesFromPasteboard:performAction:` — retained (+1 from alloc/init; no empty-init convenience ctor) and leaked prior to fix — appkit-main — cold — autorelease-helper — wrapped with `autorelease`
- [x] app/src/platform/mac/objc/services.m:42 — `forFilesFromPasteboard:performAction:` — retained (+1 from alloc/init) and leaked prior to fix — appkit-main — cold — autorelease-helper — replaced with `[NSMutableArray array]`
- [x] app/src/platform/mac/objc/services.m:58 — `labrador_register_services_provider` — retained (bare `[LabradorServicesProvider alloc]` without `init`) and leaked prior to fix; `setServicesProvider:` adds its own retain per Apple docs — appkit-main pre-event-loop (called from Rust `app_services::mac::init`) — cold (one-shot) — explicit-release — added `init`, paired with `[provider release]` after `setServicesProvider:`

## Batch 2.C — `labrador_ui-windowing-objc`

Files: `crates/labrador_ui/src/platform/mac/objc/{app.m, host_view.m, window.m, window_blur.m, fullscreen_queue.m, keycode.m}`. `window_blur.m` confirmed to have no `alloc]`/`new]`/`copy]`/`mutableCopy]` matches (CoreFoundation `CFBundleCreate`/`CFStringCreateWithCString` are already balanced by `CFRelease`). N/A.

- [x] crates/labrador_ui/src/platform/mac/objc/app.m:65 — `registerGlobalHotkey` — leaked (`setObject:forKey:` retains, but alloc+init +1 was never balanced) — appkit-main — cold — autorelease-helper — added `autorelease` so `_hotKeys` holds the only reference
- [x] crates/labrador_ui/src/platform/mac/objc/app.m:194 — `-[LabradorDelegate init]` — stored (module-level `_hotKeys` held for app lifetime; LabradorDelegate is itself deliberately leaked singleton per `get_labrador`) — appkit-main — cold — ambient — no-op, intentional singleton
- [x] crates/labrador_ui/src/platform/mac/objc/app.m:488 — `get_labrador` — stored (comment on line 483 states the delegate is deliberately leaked; guarded by `dispatch_once`) — appkit-main — cold — ambient — no-op, intentional singleton
- [x] crates/labrador_ui/src/platform/mac/objc/app.m:501 — `make_delegated_menu` — autoreleased — appkit-main — cold — autorelease-helper — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/app.m:509 — `make_services_menu_item` — leaked (`NSApp.servicesMenu` setter retains; alloc+init +1 was never balanced) — appkit-main — cold — autorelease-helper — added `autorelease`
- [x] crates/labrador_ui/src/platform/mac/objc/app.m:512 — `make_services_menu_item` — leaked (returned from factory; caller stores `submenu` which retains) — appkit-main — cold — autorelease-helper — added `autorelease` so the factory matches the rest of the menu-factory conventions in this file
- [x] crates/labrador_ui/src/platform/mac/objc/app.m:524 — `make_labrador_custom_menu_item` — autoreleased — appkit-main — cold — autorelease-helper — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/app.m:527 — `make_labrador_custom_menu_item` — autoreleased — appkit-main — cold — autorelease-helper — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/host_view.m:281 — `-[LabradorHostView initWithFrame:...]` — stored (`markedText` ivar, released in `dealloc`) — appkit-main — cold — explicit-release — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/host_view.m:282 — `-[LabradorHostView initWithFrame:...]` — leaked (`textToInsert` ivar was not released in `dealloc`) — appkit-main — cold — explicit-release — added `[textToInsert release]` to `-dealloc`
- [x] crates/labrador_ui/src/platform/mac/objc/host_view.m:423 — `-insertText:replacementRange:` — released (explicit `[characters release]` at line 445) — appkit-event — hot — explicit-release — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/host_view.m:470 — `-setMarkedText:...` — stored (`markedText` ivar; previous value released at line 468, final release in `dealloc`) — appkit-event — hot — explicit-release — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/host_view.m:472 — `-setMarkedText:...` — stored (same pattern as :470) — appkit-event — hot — explicit-release — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:37 — `-enqueueFullscreenTransition` — stored (module-level `fullscreenManager` via `dispatch_once`, intentional singleton) — appkit-main — cold — ambient — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:499 — `+[LabradorWindow createWithContentRect:...]` — retained and returned per the `create` naming convention (caller owns); Rust `Window::open` stores the resulting `id` as `native_window` and AppKit releases it via `releasedWhenClosed = YES` — appkit-main — cold — ambient — no-op, documented ownership transfer
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:663 — `+[LabradorPanel createWithContentRect:...]` — same as :499 (ownership transferred to Rust caller) — appkit-main — cold — ambient — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:689 — `create_labrador_nspanel` — released (manually balanced by `[pool release]` at line 714/719 post-edit) — appkit-main — cold — local-pool — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:693 — `create_labrador_nspanel` — stored (module-level `windowOrderForTests` via `dispatch_once`, intentional singleton for integration tests) — appkit-main — cold — ambient — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:703 — `create_labrador_nspanel` — autoreleased — appkit-main — cold — autorelease-helper — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:708 — `create_labrador_nspanel` — leaked (`NSWindow.delegate` is weak; the +1 retain count was never balanced so the delegate outlived every window open) — appkit-main — cold — stored — tied delegate lifetime to window via `objc_setAssociatedObject` + released caller's +1
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:721 — `create_labrador_nswindow` — released (manually balanced by `[pool release]` at line 746/753 post-edit) — appkit-main — cold — local-pool — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:725 — `create_labrador_nswindow` — stored (same as :693) — appkit-main — cold — ambient — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:735 — `create_labrador_nswindow` — autoreleased — appkit-main — cold — autorelease-helper — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/window.m:740 — `create_labrador_nswindow` — leaked (same root cause as :708) — appkit-main — cold — stored — fixed alongside :708 with `objc_setAssociatedObject`
- [x] crates/labrador_ui/src/platform/mac/objc/fullscreen_queue.m:17 — `-[FullscreenWindowManager init]` — stored (ivar on `fullscreenManager` singleton which is itself intentionally leaked for app lifetime) — appkit-main — cold — ambient — no-op
- [x] crates/labrador_ui/src/platform/mac/objc/keycode.m:163 — `charToKeyCodes` — stored (module-level `keycodeDict` cache, intentional singleton built lazily on first call) — rust-thread? — cold — ambient — no-op, singleton cache
- [x] crates/labrador_ui/src/platform/mac/objc/keycode.m:193 — `charToKeyCodes` — leaked (`setObject:forKey:` retains; alloc+init +1 was never balanced) — rust-thread? — cold — autorelease-helper — added `autorelease`
- [x] crates/labrador_ui/src/platform/mac/objc/keycode.m:201 — `charToKeyCodes` — leaked (same pattern as :193) — rust-thread? — cold — autorelease-helper — added `autorelease`

## Batch 2.D — `labrador_ui-chrome-objc`

Files: `crates/labrador_ui/src/platform/mac/objc/{alert.m, menus.m, notifications/notifications.m, reachability.m, hotkey.m}`. `alert.m`, `menus.m` (beyond `[super dealloc]`), and `hotkey.m` currently have no `alloc]` matches; agent confirms.

Confirmed via `rg -n 'alloc\]|\bnew\]|\bcopy\]|\bmutableCopy\]'` on each file in the working tree: `alert.m`, `hotkey.m` → no matches (N/A). `menus.m` → only `[super dealloc]` at line 25 (N/A).

- [x] crates/labrador_ui/src/platform/mac/objc/notifications/notifications.m:55 — `sendNotificationWithErrorHandler` completion block — leaked (alloc/init `UNMutableNotificationContent` never released) — gcd-block (UNUserNotificationCenter completion handler) — cold (per user-triggered notification) — autorelease-helper — added inline `autorelease` on the alloc/init expression
- [x] crates/labrador_ui/src/platform/mac/objc/reachability.m:93 — `+reachabilityWithHostname:` — autoreleased — appkit-main (via `labrador_will_finish_launching` → `setReachabilityListener`) — cold (once per app lifetime) — autorelease-helper — added `autorelease` so the factory matches Cocoa naming conventions; caller in `app.m:394` now `retain`s and `-[LabradorDelegate dealloc]` calls `stopNotifier` (to break the `reachabilityObject = self` retain cycle set up by `-startNotifier`) followed by `release`.
- [x] crates/labrador_ui/src/platform/mac/objc/reachability.m:105 — `+reachabilityWithAddress:` — autoreleased — n/a (dead path today; only reached via `reachabilityForInternetConnection` / `reachabilityForLocalWiFi` / `reachabilityWithURL`, none of which are called in the current tree) — cold — autorelease-helper — added `autorelease` alongside :93 for consistency; no caller updates required because the path is unused today.

## Batch 2.E — `rust-msg-send-alloc`

Rust-side `msg_send![class!(X), alloc]` sites that retain without autoreleasing. These require explicit balance or switching to an autoreleased helper.

`crates/labrador_ui_extras/src/user_preferences/user_defaults.rs:39` is adjacent to NSString lines audited by batch 1.D; to avoid merge conflicts it's owned by batch 1.D in `nsstring_checklist.md`, not this file.

- [x] crates/labrador_ui/src/platform/mac/app.rs:46 — `NSAlert::alloc` trait impl — autoreleased (by caller) — appkit-main — cold — autorelease-helper — no-op: caller at :80 `create_native_platform_modal` wraps the chain in `NSAlert::autorelease(NSAlert::init(NSAlert::alloc(nil)))`, and the caller's callers (`show_native_platform_modal` in `delegate.rs:375`) run on the AppKit main thread where an ambient pool exists
- [x] crates/labrador_ui/src/platform/mac/app.rs:187 — `App::run` — retained (chained into `initWithBytes_length_`) — appkit-main — cold — ambient — no-op: `App::run` is a one-shot called from `main`; `NSAutoreleasePool::new(nil)` at :178 spans the entire NSApp run loop and drains at :210 on app shutdown. The icon data is consumed synchronously by the `NSImage` init at :192, and the resulting image is retained by `NSApp` via `setApplicationIconImage:` at :206. Any residual retain is reclaimed at process exit.
- [x] crates/labrador_ui/src/platform/mac/app.rs:192 — `App::run` — retained (chained into `initWithData_`) — appkit-main — cold — ambient — no-op: same scope as :187. The produced image is handed off to `NSApp` via `setApplicationIconImage:` (NSApp retains) at :206, and the outer pool at :178 covers the call; one-shot at startup, reclaimed on process exit.
- [x] crates/labrador_ui/src/platform/mac/clipboard.rs:68 — `<impl Clipboard for Clipboard>::write` — leaked (chained `alloc].initWithBytes_length_` never balanced; pasteboard retains its own copy) — appkit-main — cold (user-initiated copy action, not a tight loop) — explicit-release — added `msg_send![data, release]` after `setData:forType:` to balance the `+1` from `[NSData alloc]`; pasteboard retain keeps the data alive for consumers
- [x] app/src/appearance.rs:234 — `AppearanceManager::set_app_icon` — leaked (chained `alloc].initWithContentsOfFile:` never balanced) — mixed (appkit-main from settings UI + `ctx.spawn` continuation after autoupdate + app init in `lib.rs:1204`) — cold (fires on icon change, app start, after updates) — explicit-release — added `msg_send![image, release]` after the final `noteFileSystemChanged:`; `setApplicationIconImage:` and `setIcon:forFile:options:` both retain the image, and `initWithContentsOfFile:` releases the `alloc` on failure, so the nil-check early return needs no additional release
