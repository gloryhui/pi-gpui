mod attachments;
#[cfg(all(debug_assertions, target_os = "macos"))]
mod automation;
mod chat;
mod diff;
mod dock;
mod file_tree;
mod highlight;
mod hub;
mod jsonl;
mod keys;
mod markdown;
mod open_url;
mod pi;
mod pi_updates;
mod platform;
mod plugins;
mod selectable;
mod sessions;
mod settings;
#[cfg(not(target_os = "macos"))]
mod single_instance;
mod text_input;
mod ui;
mod updater;
mod window_frame;

use std::path::PathBuf;

use futures::StreamExt;

use gpui::{
    App, Application, Bounds, Focusable, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType,
    TitlebarOptions, WindowBounds, WindowOptions, actions, point, prelude::*, px, size,
};

use crate::hub::HubView;
use crate::ui::Assets;

actions!(
    app,
    [Quit, CheckForUpdates, About, Hide, HideOthers, ShowAll]
);

fn main() {
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("pi-gpui {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // Release tooling: the pi versions this build supports, as JSON.
    if std::env::args().nth(1).as_deref() == Some("--pi-compat") {
        println!("{}", pi::compatibility_json());
        return;
    }
    // Usage: pi-gpui [workspace | pi-gpui://link]. A workspace is added to
    // the sidebar and a new session is opened in it. Linux link handlers pass
    // the link as the argument (resources/pi-gpui.desktop).
    let initial_link = std::env::args()
        .nth(1)
        .filter(|argument| argument.starts_with("pi-gpui:") || argument.starts_with("pi-gpui-zh:"));
    let initial_workspace = std::env::args_os()
        .nth(1)
        .filter(|_| initial_link.is_none())
        .map(PathBuf::from)
        .map(|path| match path.canonicalize() {
            Ok(path) if path.is_dir() => path,
            Ok(path) => {
                eprintln!("pi-gpui: {} is not a directory", path.display());
                std::process::exit(1);
            }
            Err(error) => {
                eprintln!("pi-gpui: {}: {error}", path.display());
                std::process::exit(1);
            }
        });

    // pi-gpui:// links (Info.plist registers the scheme). macOS can deliver
    // them before the window exists, so they wait in a channel.
    let (url_sender, mut url_receiver) = futures::channel::mpsc::unbounded::<Vec<String>>();
    #[cfg(not(target_os = "macos"))]
    {
        let message = match (&initial_link, &initial_workspace) {
            (Some(link), _) => link.clone(),
            (None, Some(workspace)) => open_url::workspace_link(workspace),
            (None, None) => single_instance::ACTIVATE.to_string(),
        };
        if single_instance::forward(&message) {
            return;
        }
        let sender = url_sender.clone();
        single_instance::listen(move |message| {
            sender.unbounded_send(vec![message]).ok();
        });
    }
    if let Some(link) = &initial_link {
        url_sender.unbounded_send(vec![link.clone()]).ok();
    }
    let app = Application::new().with_assets(Assets);
    app.on_open_urls(move |urls| {
        url_sender.unbounded_send(urls).ok();
    });
    app.run(move |cx: &mut App| {
        settings::init(cx);
        text_input::bind_keys(cx);
        chat::bind_keys(cx);
        file_tree::bind_keys(cx);
        hub::bind_keys(cx);
        cx.bind_keys([KeyBinding::new(&crate::keys::binding("cmd-q"), Quit, None)]);
        #[cfg(target_os = "macos")]
        cx.bind_keys([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
        ]);
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &About, cx| {
            cx.open_url("https://github.com/bytesbay/pi-gpui");
        });
        cx.on_action(|_: &Quit, cx| cx.quit());
        updater::AutoUpdater::init(cx);
        highlight::preload();
        cx.on_action(|_: &CheckForUpdates, cx| {
            updater::AutoUpdater::global(cx).update(cx, |updater, cx| updater.check(true, cx));
        });
        cx.set_menus(app_menus());
        #[cfg(target_os = "macos")]
        platform::localize_window_menu();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // Draw our own title bar; macOS keeps the traffic lights.
            titlebar: Some(TitlebarOptions {
                title: Some("Pi GPUI 中文版".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(11.))),
            }),
            // Linux: draw the frame ourselves where the compositor allows it
            // (window_frame.rs); app_id matches the .desktop file for the icon.
            window_decorations: (!cfg!(target_os = "macos"))
                .then_some(gpui::WindowDecorations::Client),
            app_id: Some("pi-gpui".into()),
            ..Default::default()
        };

        let window = match cx.open_window(options, |window, cx| {
            cx.new(|cx| HubView::new(initial_workspace.clone(), window, cx))
        }) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("pi-gpui: failed to open a window: {error:#}");
                cx.quit();
                return;
            }
        };

        // A window driven by automation stays behind the user's work.
        #[cfg(all(debug_assertions, target_os = "macos"))]
        let activate = automation::folder().is_none();
        #[cfg(not(all(debug_assertions, target_os = "macos")))]
        let activate = true;
        let focused = window.update(cx, |view, window, cx| {
            window.focus(&view.focus_handle(cx));
            if activate {
                cx.activate(true);
            }
        });
        if let Err(error) = focused {
            eprintln!("pi-gpui: failed to focus the window: {error:#}");
        }
        #[cfg(all(debug_assertions, target_os = "macos"))]
        automation::start(window.into(), cx);

        cx.spawn(async move |cx| {
            while let Some(urls) = url_receiver.next().await {
                let opened = window.update(cx, |view, window, cx| {
                    // "activate" (single_instance.rs) only brings the window forward.
                    for url in urls.iter().filter(|url| url.as_str() != "activate") {
                        view.open_url(url, cx);
                    }
                    window.activate_window();
                });
                if opened.is_err() {
                    break;
                }
                cx.update(|cx| cx.activate(true)).ok();
            }
        })
        .detach();
    });
}

/// The macOS menu bar, laid out like Zed's.
fn app_menus() -> Vec<Menu> {
    use crate::text_input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    vec![
        Menu {
            name: "Pi GPUI".into(),
            items: vec![
                MenuItem::action("关于 Pi GPUI 中文版", About),
                MenuItem::action("检查更新…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("设置…", hub::OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("服务", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("隐藏 Pi GPUI 中文版", Hide),
                MenuItem::action("隐藏其他应用", HideOthers),
                MenuItem::action("显示全部", ShowAll),
                MenuItem::separator(),
                MenuItem::action("退出 Pi GPUI 中文版", Quit),
            ],
        },
        Menu {
            name: "文件".into(),
            items: vec![
                MenuItem::action("新建会话", hub::NewChat),
                MenuItem::action("关闭会话", hub::CloseChat),
                MenuItem::action("重新加载所有会话", hub::ReloadSessions),
                MenuItem::action("添加项目…", hub::AddWorkspace),
            ],
        },
        Menu {
            name: "编辑".into(),
            items: vec![
                MenuItem::os_action("撤销", Undo, OsAction::Undo),
                MenuItem::os_action("重做", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("剪切", Cut, OsAction::Cut),
                MenuItem::os_action("复制", Copy, OsAction::Copy),
                MenuItem::os_action("粘贴", Paste, OsAction::Paste),
                MenuItem::os_action("全选", SelectAll, OsAction::SelectAll),
            ],
        },
        Menu {
            name: "显示".into(),
            items: vec![
                MenuItem::action("左侧面板", hub::ToggleLeftDock),
                MenuItem::action("右侧面板", hub::ToggleRightDock),
                MenuItem::action("文件", hub::ToggleFiles),
                MenuItem::action("改动", hub::ToggleChanges),
                MenuItem::separator(),
                MenuItem::action("会话树", chat::ToggleTree),
                MenuItem::action("切换模型", chat::ToggleModelPicker),
                MenuItem::action("展开或折叠步骤详情", hub::ToggleDetails),
                MenuItem::separator(),
                MenuItem::action("进入全屏", hub::ToggleFullScreen),
            ],
        },
        Menu {
            name: if cfg!(target_os = "macos") {
                "Window"
            } else {
                "窗口"
            }
            .into(),
            items: vec![
                MenuItem::action("最小化", hub::Minimize),
                MenuItem::action("缩放", hub::Zoom),
            ],
        },
    ]
}
