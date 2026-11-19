//! Every file of the web app, as it is built into this crate.

/// One file of the web app: its path under `ui/` and its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiFile {
    /// The path under `ui/`, with `/` between components (`pages/fleet.js`).
    pub path: &'static str,
    /// The file's text.
    pub text: &'static str,
}

impl UiFile {
    /// The file this crate holds at `path`, if any.
    pub fn named(path: &str) -> Option<UiFile> {
        UI_FILES.iter().copied().find(|f| f.path == path)
    }

    /// Whether the file is a JavaScript module.
    pub fn is_script(&self) -> bool {
        self.path.ends_with(".js")
    }

    /// Whether the file is part of a vendored library (under `ui/vendor/`).
    pub fn is_vendored(&self) -> bool {
        crate::vendor::is_vendored(self.path)
    }
}

macro_rules! ui_files {
    ($($path:literal),* $(,)?) => {
        /// Every file under `ui/`. A test lists the directory and fails when a file is missing
        /// here, so no file escapes the scans.
        pub const UI_FILES: &[UiFile] = &[
            $(UiFile { path: $path, text: include_str!(concat!("../../../ui/", $path)) },)*
        ];
    };
}

ui_files![
    "index.html",
    "app.css",
    "lab.css",
    "styles/host.css",
    "styles/forms.css",
    "styles/enrollment.css",
    "styles/gear.css",
    "styles/sleds.css",
    "packages.css",
    "package.css",
    "calls.json",
    "api.js",
    "app.js",
    "bytes.js",
    "lab.js",
    "sleds.js",
    "sledpanels.js",
    "openpgp.js",
    "main.js",
    "socket_address.js",
    "session.js",
    "signin.js",
    "view.js",
    "sshpanel.js",
    "terminal.js",
    "terminal_render.js",
    "terminal_keys.js",
    "terminal_port.js",
    "styles/terminal.css",
    "styles/serialconsole.css",
    "vendor/xterm/xterm.js",
    "vendor/xterm/xterm.css",
    "vendor/xterm/LICENSE",
    "vendor/xterm-addon-attach/addon-attach.js",
    "vendor/xterm-addon-attach/LICENSE",
    "gen/client.js",
    "gen/client.d.ts",
    "gen/hostkeeper_agent_v1.d.ts",
    "gen/hostkeeper_operator_v1.d.ts",
    "gen/hostkeeper_release_v1.d.ts",
    "pages/audit.js",
    "pages/enrollment.js",
    "pages/holds.js",
    "pages/home.js",
    "pages/hostgroups.js",
    "pages/host.js",
    "pages/inventory.js",
    "pages/package.js",
    "pages/packages.js",
    "pages/release.js",
    "pages/switch.js",
    "pages/rollout.js",
    "pages/serialconsole.js",
    "pages/tokens.js",
    "pages/users.js",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_found_by_its_path_and_a_missing_one_is_none() {
        let api = UiFile::named("api.js").expect("api.js is held");
        assert!(api.is_script());
        assert!(api.text.contains("export function createApi"));
        let html = UiFile::named("index.html").expect("index.html is held");
        assert!(!html.is_script());
        assert_eq!(UiFile::named("pages/missing.js"), None);
        assert!(!api.is_vendored());
        let vendored = UiFile {
            path: "vendor/lib/lib.js",
            text: "",
        };
        assert!(vendored.is_vendored());
    }
}
