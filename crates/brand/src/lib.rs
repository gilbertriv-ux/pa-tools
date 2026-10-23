//! The product's identity in one place (layer 0, no dependencies).
//!
//! Pa Tools is a fork of SoundCraft. Crate names, module paths, the `.scraft` session extension and
//! the `SOUNDCRAFT_*` environment variables stay as upstream has them, so upstream merges stay small
//! and sessions stay interchangeable. Everything a person can read or see (window titles, menus, the
//! About window, file-dialog filters, log and settings folders, the MCP server name, plugin-host
//! identity, Windows resource strings) comes from the constants below instead of a string literal.
//!
//! After merging upstream, `grep -rn --include=*.rs -E '"[^"]*(SoundCraft|ArtCraft)[^"]*"' crates apps`
//! finds any new upstream brand string that still needs routing through this crate.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::ffi::CStr;

/// The product name as shown in titles, menus, dialogs and the About window.
pub const APP_NAME: &str = "Pa Tools";
/// One-line description for the About window and package metadata.
pub const DESCRIPTION: &str = "A professional multitrack audio workstation by GR8 ID YUZ.AI.";
/// Reverse-DNS application id (Wayland/X11 app id, macOS bundle id, Flatpak id).
pub const APP_ID: &str = "com.imagescouture.patools";
/// The company that ships the product.
pub const VENDOR: &str = "GR8 ID YUZ.AI";
/// Publisher line for installers and store listings.
pub const PUBLISHER: &str = "Images Couture · GR8 ID YUZ.AI";
/// Product home page. Also the URL a plugin sees in the host description.
pub const HOMEPAGE: &str = "https://gr8idyuz.ai"; // placeholder until Gil confirms

/// Settings folder name on macOS (`~/Library/Application Support/<name>`) and Windows (`%APPDATA%\<name>`).
pub const CONFIG_DIR_NAME: &str = "Pa Tools";
/// Settings folder name under `$XDG_CONFIG_HOME` (or `~/.config`) on Linux and the other Unixes.
pub const CONFIG_DIR_XDG: &str = "patools";
/// Portable mode: the folder beside the executable that holds settings, autosaves, presets and logs.
pub const PORTABLE_DATA_DIR: &str = "PaToolsData";
/// The current log file's name inside the log folder.
pub const LOG_FILE: &str = "patools.log";
/// Rotated logs are `<stem>.1.log`, `<stem>.2.log`, … (the stem of [`LOG_FILE`]).
pub const LOG_FILE_STEM: &str = "patools";

/// `serverInfo.name` an MCP client sees.
pub const MCP_SERVER_NAME: &str = "pa-tools";
/// URI scheme of the MCP resources.
pub const URI_SCHEME: &str = "patools";
/// MCP resource: the open session as JSON.
pub const RESOURCE_SESSION: &str = "patools://session";
/// MCP resource: every command.
pub const RESOURCE_COMMANDS: &str = "patools://commands";

/// Environment variable that starts the desktop app's control channel on a port.
pub const CONTROL_PORT_ENV: &str = "PATOOLS_CONTROL_PORT";
/// The upstream spelling of [`CONTROL_PORT_ENV`], still read when the new one is unset so existing
/// scripts keep working.
pub const LEGACY_CONTROL_PORT_ENV: &str = "SOUNDCRAFT_CONTROL_PORT";

/// File-dialog filter name for session files (`.scraft`).
pub const SESSION_LABEL: &str = "Pa Tools Session";
/// Command line tool name.
pub const CLI_NAME: &str = "pa-tools-cli";
/// Desktop app executable name (without the platform extension).
pub const BIN_NAME: &str = "pa-tools";
/// The vendor a plugin sees in the host description.
pub const HOST_VENDOR: &str = "GR8 ID YUZ.AI";
/// The credit the upstream brand licence allows, in plain text.
pub const BASED_ON: &str = "Pa Tools is based on SoundCraft by the ArtCraft team (MIT OR Apache-2.0).";
/// The upstream project, for the credit line.
pub const UPSTREAM_URL: &str = "https://github.com/storytold/soundcraft";
/// Copyright line for Windows file properties and package metadata.
pub const LEGAL_COPYRIGHT: &str = "Copyright (c) GR8 ID YUZ.AI. Pa Tools is based on SoundCraft by the ArtCraft team (MIT OR Apache-2.0).";

/// Prefix of the private tags and properties Pa Tools writes into AAF and OMF files (`"<prefix> Fade
/// Shape"`, `"<prefix>:SCLP:ClipName"`). Other applications show them as plain metadata, and readers
/// of our own files look for them by this name, so it stays fixed once files exist.
pub const INTERCHANGE_PREFIX: &str = "PaTools";

/// Windows window class of the CLAP plugin editor host window.
pub const CLAP_EDITOR_CLASS: &str = "PaToolsClapEditor";
/// Windows window class of the VST3 plugin editor host window.
pub const VST3_EDITOR_CLASS: &str = "PaToolsVst3Editor";

// NUL-terminated copies for the plugin-host FFI (C string literals are checked at compile time, so
// they cost nothing at run time and need no `unsafe`; the tests below keep them equal to the above).
/// [`APP_NAME`] as a C string (the CLAP host name).
pub const APP_NAME_C: &CStr = c"Pa Tools";
/// [`HOST_VENDOR`] as a C string (the CLAP host vendor).
pub const HOST_VENDOR_C: &CStr = c"GR8 ID YUZ.AI";
/// [`HOMEPAGE`] as a C string (the CLAP host url).
pub const HOMEPAGE_C: &CStr = c"https://gr8idyuz.ai"; // placeholder until Gil confirms

/// The port requested through the environment: [`CONTROL_PORT_ENV`], else [`LEGACY_CONTROL_PORT_ENV`].
/// An empty value counts as unset.
pub fn control_port_env() -> Option<String> {
    [CONTROL_PORT_ENV, LEGACY_CONTROL_PORT_ENV].iter().filter_map(|name| std::env::var(name).ok()).find(|v| !v.trim().is_empty())
}

/// The main window title. With the system menu bar (macOS) the title also carries the dirty mark and
/// the front window, since no egui bar shows them.
pub fn window_title(session: &str, dirty: bool, front_window: &str, with_state: bool) -> String {
    if with_state {
        format!("{session}{} — {APP_NAME} — {front_window}", if dirty { " *" } else { "" })
    } else {
        format!("{session} — {APP_NAME}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_strings_match_their_rust_twins() {
        assert_eq!(APP_NAME_C.to_str(), Ok(APP_NAME));
        assert_eq!(HOST_VENDOR_C.to_str(), Ok(HOST_VENDOR));
        assert_eq!(HOMEPAGE_C.to_str(), Ok(HOMEPAGE));
    }

    #[test]
    fn derived_names_stay_consistent() {
        assert_eq!(LOG_FILE, format!("{LOG_FILE_STEM}.log"));
        assert_eq!(RESOURCE_SESSION, format!("{URI_SCHEME}://session"));
        assert_eq!(RESOURCE_COMMANDS, format!("{URI_SCHEME}://commands"));
        assert_eq!(SESSION_LABEL, format!("{APP_NAME} Session"));
        assert_eq!(CLI_NAME, format!("{BIN_NAME}-cli"));
        assert!(LEGAL_COPYRIGHT.contains(BASED_ON));
        assert!(CLAP_EDITOR_CLASS.starts_with("PaTools") && VST3_EDITOR_CLASS.starts_with("PaTools"));
    }

    #[test]
    fn user_visible_names_carry_no_upstream_or_incumbent_brand() {
        // The credit lines name the upstream on purpose; nothing else may.
        let visible = [
            APP_NAME,
            DESCRIPTION,
            APP_ID,
            VENDOR,
            PUBLISHER,
            HOMEPAGE,
            CONFIG_DIR_NAME,
            CONFIG_DIR_XDG,
            PORTABLE_DATA_DIR,
            LOG_FILE,
            MCP_SERVER_NAME,
            URI_SCHEME,
            RESOURCE_SESSION,
            RESOURCE_COMMANDS,
            CONTROL_PORT_ENV,
            SESSION_LABEL,
            CLI_NAME,
            BIN_NAME,
            HOST_VENDOR,
            CLAP_EDITOR_CLASS,
            VST3_EDITOR_CLASS,
            INTERCHANGE_PREFIX,
        ];
        for s in visible {
            let lower = s.to_lowercase();
            for banned in ["soundcraft", "artcraft", "avid", "pro tools", "storyteller", "storytold"] {
                assert!(!lower.contains(banned), "{s} contains {banned}");
            }
        }
    }

    #[test]
    fn titles() {
        assert_eq!(window_title("Song", false, "Edit", false), "Song — Pa Tools");
        assert_eq!(window_title("Song", true, "Mix", true), "Song * — Pa Tools — Mix");
        assert_eq!(window_title("Song", false, "Edit", true), "Song — Pa Tools — Edit");
    }
}
