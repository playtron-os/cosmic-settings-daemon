// SPDX-License-Identifier: MPL-2.0

use cosmic_settings_config::shortcuts::{self, SYSTEM_ACTIONS_OVERRIDE, action::System};
use std::{env, fs, path::Path};

fn write(dir: &Path, key: &str, value: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(key), value).unwrap();
}

// One test in its own binary, because it points the XDG directories at a scratch tree.
#[test]
fn override_layers_between_package_and_user() {
    let root = env::temp_dir().join(format!("cosmic-settings-config-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    // SAFETY: this binary runs a single test, so no other thread reads the environment.
    unsafe {
        env::set_var("XDG_DATA_HOME", root.join("data"));
        env::set_var("XDG_DATA_DIRS", root.join("none"));
        env::set_var("XDG_CONFIG_HOME", root.join("config"));
    }

    let system = root.join("data/cosmic").join(shortcuts::ID).join("v1");
    write(
        &system,
        "system_actions",
        r#"{ Terminal: "packaged-term", WindowSwitcher: "packaged-switcher alt-tab" }"#,
    );
    let user = shortcuts::context().unwrap();

    // Without an override the packaged commands apply.
    let actions = shortcuts::system_actions(&user);
    assert_eq!(actions[&System::Terminal], "packaged-term");
    assert_eq!(
        actions[&System::WindowSwitcher],
        "packaged-switcher alt-tab"
    );

    // A user without their own system_actions must still get the override.
    write(
        &system,
        SYSTEM_ACTIONS_OVERRIDE,
        r#"{ Terminal: "distro-term", WindowSwitcher: "distro-switcher alt-tab" }"#,
    );
    let actions = shortcuts::system_actions(&user);
    assert_eq!(actions[&System::Terminal], "distro-term");
    assert_eq!(actions[&System::WindowSwitcher], "distro-switcher alt-tab");

    write(
        &root.join("config/cosmic").join(shortcuts::ID).join("v1"),
        "system_actions",
        r#"{ Terminal: "user-term" }"#,
    );
    let actions = shortcuts::system_actions(&user);
    assert_eq!(actions[&System::Terminal], "user-term");
    assert_eq!(actions[&System::WindowSwitcher], "distro-switcher alt-tab");

    fs::remove_dir_all(&root).unwrap();
}
