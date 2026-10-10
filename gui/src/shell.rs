//! Right-click / context-menu integration for Windows Explorer, Linux file managers
//! (Nautilus, Dolphin, Nemo + .desktop MIME associations) and macOS Finder (Quick Actions).
//!
//! Generators are platform-independent pure functions so they can be unit-tested everywhere;
//! `register` / `unregister` apply them for the current OS and current user only (no admin).

#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "Archiver";

/// Final extensions (as Windows/Finder see them) of all supported archive types.
pub fn final_extensions() -> Vec<String> {
    let mut v: Vec<String> = archiver_core::archive_extensions()
        .into_iter()
        .map(|e| e.rsplit('.').next().unwrap_or(e).to_string())
        .collect();
    v.sort();
    v.dedup();
    v
}

pub const MIME_TYPES: &[&str] = &[
    "application/zip",
    "application/x-zip-compressed",
    "application/java-archive",
    "application/x-7z-compressed",
    "application/vnd.rar",
    "application/x-rar",
    "application/x-rar-compressed",
    "application/x-tar",
    "application/x-compressed-tar",
    "application/x-bzip-compressed-tar",
    "application/x-bzip2-compressed-tar",
    "application/x-xz-compressed-tar",
    "application/x-zstd-compressed-tar",
    "application/gzip",
    "application/x-gzip",
    "application/x-bzip",
    "application/x-bzip2",
    "application/x-xz",
    "application/zstd",
];

/// One menu command: (id, label, CLI flag, archives only?)
pub const COMMANDS: &[(&str, &str, &str, bool)] = &[
    ("open", "Open with Archiver", "--open", true),
    ("extracthere", "Extract here", "--extract-here", true),
    ("extractto", "Extract to folder", "--extract-to-folder", true),
    ("compress", "Add to archive…", "--compress", false),
];

// ------------------------------------------------------------------ Windows

/// A registry key (relative to HKCU) and its string values ("" = default value).
#[derive(Debug, Clone, PartialEq)]
pub struct RegKey {
    pub path: String,
    pub values: Vec<(String, String)>,
}

pub fn windows_keys(exe: &str, base: &str) -> Vec<RegKey> {
    let mut keys = Vec::new();
    let menu = |keys: &mut Vec<RegKey>, root: String, archive: bool, applies_to: Option<String>| {
        let mut vals = vec![
            ("MUIVerb".to_string(), APP_NAME.to_string()),
            ("SubCommands".to_string(), String::new()),
            ("Icon".to_string(), format!("\"{exe}\",0")),
        ];
        if let Some(a) = applies_to {
            vals.push(("AppliesTo".into(), a));
        }
        keys.push(RegKey { path: root.clone(), values: vals });
        for (i, (id, label, flag, arc_only)) in COMMANDS.iter().enumerate() {
            if *arc_only && !archive {
                continue;
            }
            let label = if *id == "extractto" { "Extract to folder\\".to_string() } else { label.to_string() };
            let sub = format!("{root}\\shell\\{:02}{id}", i + 1);
            keys.push(RegKey { path: sub.clone(), values: vec![("MUIVerb".into(), label)] });
            keys.push(RegKey {
                path: format!("{sub}\\command"),
                values: vec![(String::new(), format!("\"{exe}\" {flag} \"%1\""))],
            });
        }
    };
    let exts = final_extensions();
    for e in &exts {
        menu(&mut keys, format!("{base}\\SystemFileAssociations\\.{e}\\shell\\Archiver"), true, None);
    }
    // all other files: only "Add to archive"
    let not = exts.iter().map(|e| format!("System.FileExtension:=.{e}")).collect::<Vec<_>>().join(" OR ");
    menu(&mut keys, format!("{base}\\*\\shell\\Archiver"), false, Some(format!("NOT ({not})")));
    menu(&mut keys, format!("{base}\\Directory\\shell\\Archiver"), false, None);
    keys
}

/// Top-level keys to delete on unregister.
pub fn windows_roots(base: &str) -> Vec<String> {
    let mut v: Vec<String> = final_extensions()
        .iter()
        .map(|e| format!("{base}\\SystemFileAssociations\\.{e}\\shell\\Archiver"))
        .collect();
    v.push(format!("{base}\\*\\shell\\Archiver"));
    v.push(format!("{base}\\Directory\\shell\\Archiver"));
    v
}

#[cfg(windows)]
mod win {
    use super::*;
    use winreg::RegKey as WKey;
    use winreg::enums::HKEY_CURRENT_USER;

    pub fn apply(exe: &str, base: &str) -> std::io::Result<()> {
        let hkcu = WKey::predef(HKEY_CURRENT_USER);
        for k in windows_keys(exe, base) {
            let (key, _) = hkcu.create_subkey(&k.path)?;
            for (n, v) in &k.values {
                key.set_value(n, v)?;
            }
        }
        notify();
        Ok(())
    }

    pub fn remove(base: &str) -> std::io::Result<()> {
        let hkcu = WKey::predef(HKEY_CURRENT_USER);
        for r in windows_roots(base) {
            match hkcu.delete_subkey_all(&r) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        notify();
        Ok(())
    }

    /// Delete keys if present; returns how many existed.
    pub fn remove_roots(roots: &[String]) -> usize {
        let hkcu = WKey::predef(HKEY_CURRENT_USER);
        roots.iter().filter(|r| hkcu.delete_subkey_all(r.as_str()).is_ok()).count()
    }

    pub fn exists(base: &str) -> bool {
        WKey::predef(HKEY_CURRENT_USER).open_subkey(format!("{base}\\Directory\\shell\\Archiver")).is_ok()
    }

    pub fn read_default(path: &str) -> std::io::Result<String> {
        WKey::predef(HKEY_CURRENT_USER).open_subkey(path)?.get_value("")
    }

    fn notify() {
        // SHChangeNotify(SHCNE_ASSOCCHANGED) is not strictly needed for shell verbs;
        // Explorer re-reads HKCU\Software\Classes on the next right-click.
    }
}

// ------------------------------------------------------------------ Linux

/// A file to write: (path relative to $HOME, content, executable).
pub type GenFile = (PathBuf, String, bool);

fn desktop_quote(exe: &str) -> String {
    // Desktop Entry spec: quote and escape ", `, $ and \ inside quotes.
    let mut s = String::from("\"");
    for c in exe.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn linux_files(exe: &str) -> Vec<GenFile> {
    let q = desktop_quote(exe);
    let mimes = format!("{};", MIME_TYPES.join(";"));
    let mut out: Vec<GenFile> = Vec::new();

    // 1) application .desktop with MIME associations ("Open With") and actions
    out.push((
        ".local/share/applications/archiver.desktop".into(),
        format!(
            "[Desktop Entry]\nType=Application\nName={APP_NAME}\nGenericName=Archive Manager\nComment=Create and extract archives (ZIP, 7z, TAR, RAR...)\n\
Exec={q} %F\nIcon=package-x-generic\nTerminal=false\nCategories=Utility;Archiving;Compression;\nMimeType={mimes}\n\
Actions=ExtractHere;ExtractToFolder;Compress;\n\n\
[Desktop Action ExtractHere]\nName=Extract here\nExec={q} --extract-here %F\n\n\
[Desktop Action ExtractToFolder]\nName=Extract to folder\nExec={q} --extract-to-folder %F\n\n\
[Desktop Action Compress]\nName=Add to archive…\nExec={q} --compress %F\n"
        ),
        false,
    ));

    // 2) Nautilus (GNOME Files) scripts -> Scripts > Archiver > ...
    for (_, label, flag, _) in COMMANDS {
        let body = if *flag == "--open" {
            format!("#!/bin/sh\n# Archiver Nautilus script\nfor f in \"$@\"; do {} --open \"$f\" & done\n", sh_quote(exe))
        } else {
            format!("#!/bin/sh\n# Archiver Nautilus script\nexec {} {flag} \"$@\"\n", sh_quote(exe))
        };
        out.push((format!(".local/share/nautilus/scripts/{APP_NAME}/{label}").into(), body, true));
    }

    // 3) Dolphin service menus (KF6 location + KF5 legacy location)
    let dolphin_archive = format!(
        "[Desktop Entry]\nName={APP_NAME}\nType=Service\nServiceTypes=KonqPopupMenu/Plugin\nMimeType={mimes}\nActions=open;extractHere;extractTo;\n\
X-KDE-Submenu={APP_NAME}\nX-KDE-Priority=TopLevel\n\n\
[Desktop Action open]\nName=Open with Archiver\nIcon=package-x-generic\nExec={q} --open %f\n\n\
[Desktop Action extractHere]\nName=Extract here\nIcon=archive-extract\nExec={q} --extract-here %F\n\n\
[Desktop Action extractTo]\nName=Extract to folder\nIcon=archive-extract\nExec={q} --extract-to-folder %F\n"
    );
    let dolphin_compress = format!(
        "[Desktop Entry]\nName={APP_NAME}\nType=Service\nServiceTypes=KonqPopupMenu/Plugin\nMimeType=all/all;all/allfiles;inode/directory;\nActions=compress;\n\
X-KDE-Submenu={APP_NAME}\nX-KDE-Priority=TopLevel\n\n\
[Desktop Action compress]\nName=Add to archive…\nIcon=archive-insert\nExec={q} --compress %F\n"
    );
    for dir in [".local/share/kio/servicemenus", ".local/share/kservices5/ServiceMenus"] {
        out.push((format!("{dir}/archiver-archive.desktop").into(), dolphin_archive.clone(), true));
        out.push((format!("{dir}/archiver-compress.desktop").into(), dolphin_compress.clone(), true));
    }

    // 4) Nemo (Cinnamon) actions
    let exts = archiver_core::archive_extensions().join(";");
    for (id, label, flag, arc_only) in COMMANDS {
        let (sel, filter) = if *arc_only {
            (if *flag == "--open" { "s" } else { "notnone" }, format!("Extensions={exts};"))
        } else {
            ("notnone", "Extensions=any;".to_string())
        };
        out.push((
            format!(".local/share/nemo/actions/archiver-{id}.nemo_action").into(),
            format!(
                "[Nemo Action]\nActive=true\nName={APP_NAME}: {label}\nComment={label}\nExec={q} {flag} %F\nIcon-Name=package-x-generic\nSelection={sel}\n{filter}\n"
            ),
            false,
        ));
    }
    out
}

/// Extra paths (directories) removed on unregister on Linux.
fn linux_extra_dirs() -> Vec<PathBuf> {
    vec![format!(".local/share/nautilus/scripts/{APP_NAME}").into()]
}

// ------------------------------------------------------------------ macOS

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const ARCHIVE_UTIS: &[&str] = &[
    "public.archive",
    "public.zip-archive",
    "com.pkware.zip-archive",
    "org.7-zip.7-zip-archive",
    "com.rarlab.rar-archive",
    "public.tar-archive",
    "org.gnu.gnu-tar-archive",
    "org.gnu.gnu-zip-archive",
    "org.gnu.gnu-zip-tar-archive",
    "public.bzip2-archive",
    "public.tar-bzip2-archive",
    "org.tukaani.xz-archive",
    "org.tukaani.tar-xz-archive",
    "public.zstd-archive",
];

pub fn macos_workflow_name(label: &str) -> String {
    format!("{APP_NAME} - {}", label.trim_end_matches('…'))
}

/// Generate Quick Action bundles: files under ~/Library/Services/<name>.workflow/Contents/.
pub fn macos_files(exe: &str) -> Vec<GenFile> {
    let mut out = Vec::new();
    for (_, label, flag, arc_only) in COMMANDS {
        let name = macos_workflow_name(label);
        let dir = PathBuf::from(format!("Library/Services/{name}.workflow/Contents"));
        let types: Vec<&str> = if *arc_only { ARCHIVE_UTIS.to_vec() } else { vec!["public.item"] };
        let types_xml: String = types.iter().map(|t| format!("\t\t\t\t<string>{t}</string>\n")).collect();
        let info = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSServices</key>
	<array>
		<dict>
			<key>NSMenuItem</key>
			<dict>
				<key>default</key>
				<string>{name}</string>
			</dict>
			<key>NSMessage</key>
			<string>runWorkflowAsService</string>
			<key>NSRequiredContext</key>
			<dict>
				<key>NSApplicationIdentifier</key>
				<string>com.apple.finder</string>
			</dict>
			<key>NSSendFileTypes</key>
			<array>
{types_xml}			</array>
		</dict>
	</array>
</dict>
</plist>
"#,
            name = xml_escape(&name)
        );
        let cmd = if *flag == "--open" {
            format!("for f in \"$@\"; do {} --open \"$f\" &amp;&gt;/dev/null &amp; done", xml_escape(&sh_quote(exe)))
        } else {
            format!("{} {flag} \"$@\" &amp;&gt;/dev/null &amp;", xml_escape(&sh_quote(exe)))
        };
        let wflow = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key>
	<string>523</string>
	<key>AMApplicationVersion</key>
	<string>2.10</string>
	<key>AMDocumentVersion</key>
	<string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Optional</key>
					<true/>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>AMActionVersion</key>
				<string>2.0.3</string>
				<key>AMApplication</key>
				<array>
					<string>Automator</string>
				</array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key>
					<dict/>
					<key>CheckedForUserDefaultShell</key>
					<dict/>
					<key>inputMethod</key>
					<dict/>
					<key>shell</key>
					<dict/>
					<key>source</key>
					<dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>ActionBundlePath</key>
				<string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key>
				<string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key>
					<string>{cmd}</string>
					<key>CheckedForUserDefaultShell</key>
					<true/>
					<key>inputMethod</key>
					<integer>1</integer>
					<key>shell</key>
					<string>/bin/bash</string>
					<key>source</key>
					<string></string>
				</dict>
				<key>BundleIdentifier</key>
				<string>com.apple.RunShellScript</string>
				<key>CFBundleVersion</key>
				<string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key>
				<false/>
				<key>CanShowWhenRun</key>
				<true/>
				<key>Category</key>
				<array>
					<string>AMCategoryUtilities</string>
				</array>
				<key>Class Name</key>
				<string>RunShellScriptAction</string>
				<key>InputUUID</key>
				<string>6E4C5C38-0D4B-4B39-9A0B-1B5E0B6B0001</string>
				<key>Keywords</key>
				<array>
					<string>Shell</string>
					<string>Script</string>
				</array>
				<key>OutputUUID</key>
				<string>6E4C5C38-0D4B-4B39-9A0B-1B5E0B6B0002</string>
				<key>UUID</key>
				<string>6E4C5C38-0D4B-4B39-9A0B-1B5E0B6B0003</string>
				<key>UnlocalizedApplications</key>
				<array>
					<string>Automator</string>
				</array>
				<key>arguments</key>
				<dict/>
				<key>isViewVisible</key>
				<integer>1</integer>
				<key>location</key>
				<string>309.000000:253.000000</string>
				<key>nibPath</key>
				<string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/Base.lproj/main.nib</string>
			</dict>
			<key>isViewVisible</key>
			<integer>1</integer>
		</dict>
	</array>
	<key>connectors</key>
	<dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>serviceInputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject</string>
		<key>serviceOutputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key>
		<integer>0</integer>
		<key>workflowTypeIdentifier</key>
		<string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#
        );
        out.push((dir.join("Info.plist"), info, false));
        out.push((dir.join("document.wflow"), wflow, false));
    }
    out
}

fn macos_extra_dirs() -> Vec<PathBuf> {
    COMMANDS.iter().map(|(_, l, _, _)| PathBuf::from(format!("Library/Services/{}.workflow", macos_workflow_name(l)))).collect()
}

// ------------------------------------------------------------------ file install helpers

pub fn write_files(home: &Path, files: &[GenFile]) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (rel, content, exec) in files {
        let p = home.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, content)?;
        #[cfg(unix)]
        if *exec {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        }
        #[cfg(not(unix))]
        let _ = exec;
        written.push(p);
    }
    Ok(written)
}

pub fn remove_files(home: &Path, files: &[GenFile], dirs: &[PathBuf]) -> std::io::Result<()> {
    for (rel, _, _) in files {
        let _ = std::fs::remove_file(home.join(rel));
    }
    for d in dirs {
        let p = home.join(d);
        if p.exists() {
            std::fs::remove_dir_all(p)?;
        }
    }
    Ok(())
}

fn home() -> std::io::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other("HOME is not set"))
}

fn exe_path() -> std::io::Result<String> {
    let p = std::env::current_exe()?;
    let p = std::fs::canonicalize(&p).unwrap_or(p);
    let s = p.display().to_string();
    // strip Windows verbatim prefix produced by canonicalize
    Ok(s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s))
}

fn run_quiet(cmd: &str, args: &[&str]) {
    let _ = std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

// ------------------------------------------------------------------ legacy (v0.2.0 "Rust Archiver") cleanup

pub const LEGACY_NAME: &str = "Rust Archiver";

/// Registry keys created by v0.2.0 (verb key "RustArchiver").
pub fn legacy_windows_roots(base: &str) -> Vec<String> {
    windows_roots(base).into_iter().map(|k| k.replace("\\shell\\Archiver", "\\shell\\RustArchiver")).collect()
}

/// Files (relative to $HOME) created by v0.2.0 on Linux.
pub fn legacy_linux_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![".local/share/applications/rust-archiver.desktop".into()];
    for dir in [".local/share/kio/servicemenus", ".local/share/kservices5/ServiceMenus"] {
        v.push(format!("{dir}/rust-archiver-archive.desktop").into());
        v.push(format!("{dir}/rust-archiver-compress.desktop").into());
    }
    for (id, _, _, _) in COMMANDS {
        v.push(format!(".local/share/nemo/actions/rust-archiver-{id}.nemo_action").into());
    }
    v
}

/// Directories (relative to $HOME) created by v0.2.0: Nautilus scripts folder and macOS Quick Actions.
pub fn legacy_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![format!(".local/share/nautilus/scripts/{LEGACY_NAME}").into()];
    for (_, label, _, _) in COMMANDS {
        let label = if *label == "Open with Archiver" { "Open with Rust Archiver" } else { label };
        v.push(format!("Library/Services/{LEGACY_NAME} - {}.workflow", label.trim_end_matches('…')).into());
    }
    v
}

/// Remove v0.2.0 file-based entries under `home`. Returns number of items removed.
pub fn remove_legacy_files(home: &Path) -> usize {
    let mut n = 0;
    for f in legacy_linux_files() {
        if std::fs::remove_file(home.join(f)).is_ok() {
            n += 1;
        }
    }
    for d in legacy_dirs() {
        if std::fs::remove_dir_all(home.join(d)).is_ok() {
            n += 1;
        }
    }
    n
}

/// Remove everything the old "Rust Archiver" (v0.2.0) registered, for the current OS.
pub fn cleanup_legacy() -> usize {
    #[cfg(windows)]
    {
        win::remove_roots(&legacy_windows_roots(WIN_BASE))
    }
    #[cfg(not(windows))]
    {
        home().map(|h| remove_legacy_files(&h)).unwrap_or(0)
    }
}

// ------------------------------------------------------------------ public API

pub const WIN_BASE: &str = r"Software\Classes";

/// Install integration for the current user. Returns a human-readable summary.
pub fn register() -> std::io::Result<String> {
    let exe = exe_path()?;
    let old = cleanup_legacy();
    if old > 0 {
        println!("Removed {old} old 'Rust Archiver' (v0.2.0) entries");
    }
    #[cfg(windows)]
    {
        win::apply(&exe, WIN_BASE)?;
        Ok(format!("Explorer context menu registered under HKCU\\{WIN_BASE} for {exe}"))
    }
    #[cfg(target_os = "macos")]
    {
        let h = home()?;
        let w = write_files(&h, &macos_files(&exe))?;
        run_quiet("/System/Library/CoreServices/pbs", &["-update"]);
        Ok(format!("Installed {} Finder Quick Action files in ~/Library/Services", w.len()))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let h = home()?;
        let w = write_files(&h, &linux_files(&exe))?;
        let apps = h.join(".local/share/applications");
        run_quiet("update-desktop-database", &[&apps.display().to_string()]);
        Ok(format!("Installed {} files (.desktop, Nautilus scripts, Dolphin service menus, Nemo actions)", w.len()))
    }
}

pub fn unregister() -> std::io::Result<String> {
    cleanup_legacy();
    #[cfg(windows)]
    {
        win::remove(WIN_BASE)?;
        Ok("Explorer context menu removed".into())
    }
    #[cfg(target_os = "macos")]
    {
        let h = home()?;
        remove_files(&h, &macos_files(""), &macos_extra_dirs())?;
        run_quiet("/System/Library/CoreServices/pbs", &["-update"]);
        Ok("Finder Quick Actions removed".into())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let h = home()?;
        remove_files(&h, &linux_files(""), &linux_extra_dirs())?;
        run_quiet("update-desktop-database", &[&h.join(".local/share/applications").display().to_string()]);
        Ok("Linux file-manager integration removed".into())
    }
}

pub fn is_registered() -> bool {
    #[cfg(windows)]
    {
        win::exists(WIN_BASE)
    }
    #[cfg(target_os = "macos")]
    {
        home().map(|h| h.join(&macos_extra_dirs()[0]).exists()).unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        home().map(|h| h.join(".local/share/applications/archiver.desktop").exists()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/opt/My Apps/archiver";

    #[test]
    fn windows_registry_layout() {
        let keys = windows_keys(r"C:\Program Files\RA\archiver.exe", WIN_BASE);
        let find = |p: &str| keys.iter().find(|k| k.path == p).unwrap_or_else(|| panic!("missing {p}"));
        let zip = find(r"Software\Classes\SystemFileAssociations\.zip\shell\Archiver");
        assert!(zip.values.contains(&("MUIVerb".into(), "Archiver".into())));
        assert!(zip.values.contains(&("SubCommands".into(), "".into())));
        let cmd = find(r"Software\Classes\SystemFileAssociations\.7z\shell\Archiver\shell\02extracthere\command");
        assert_eq!(cmd.values[0].1, r#""C:\Program Files\RA\archiver.exe" --extract-here "%1""#);
        find(r"Software\Classes\SystemFileAssociations\.rar\shell\Archiver\shell\03extractto\command");
        // generic files and folders only get "Add to archive"
        let star = find(r"Software\Classes\*\shell\Archiver");
        assert!(star.values.iter().any(|(n, v)| n == "AppliesTo" && v.contains("System.FileExtension:=.zip")));
        find(r"Software\Classes\Directory\shell\Archiver\shell\04compress\command");
        assert!(!keys.iter().any(|k| k.path.starts_with(r"Software\Classes\Directory") && k.path.contains("extract")));
        assert_eq!(windows_roots(WIN_BASE).len(), final_extensions().len() + 2);
    }

    #[test]
    fn final_exts_cover_formats() {
        let e = final_extensions();
        for x in ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "tzst"] {
            assert!(e.contains(&x.to_string()), "{x}");
        }
    }

    #[test]
    fn linux_generated_files() {
        let files = linux_files(EXE);
        let get = |p: &str| files.iter().find(|f| f.0 == Path::new(p)).unwrap_or_else(|| panic!("missing {p}"));
        let d = &get(".local/share/applications/archiver.desktop").1;
        assert!(d.contains("MimeType=application/zip;"));
        assert!(d.contains("application/x-7z-compressed"));
        assert!(d.contains("Exec=\"/opt/My Apps/archiver\" --extract-here %F"));
        let n = get(".local/share/nautilus/scripts/Archiver/Extract here");
        assert!(n.2, "nautilus script must be executable");
        assert!(n.1.contains("exec '/opt/My Apps/archiver' --extract-here \"$@\""));
        let k = &get(".local/share/kio/servicemenus/archiver-archive.desktop").1;
        assert!(k.contains("X-KDE-Submenu=Archiver") && k.contains("--extract-to-folder %F"));
        let c = &get(".local/share/kio/servicemenus/archiver-compress.desktop").1;
        assert!(c.contains("MimeType=all/all") && c.contains("--compress %F"));
        let nemo = &get(".local/share/nemo/actions/archiver-extracthere.nemo_action").1;
        assert!(nemo.contains("Extensions=zip;") || nemo.contains(";zip;"));
        assert!(nemo.contains("Selection=notnone"));
    }

    #[test]
    fn linux_install_and_uninstall_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let files = linux_files(EXE);
        write_files(t.path(), &files).unwrap();
        assert!(t.path().join(".local/share/nemo/actions/archiver-compress.nemo_action").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(t.path().join(".local/share/nautilus/scripts/Archiver/Add to archive…")).unwrap();
            assert_eq!(m.permissions().mode() & 0o111, 0o111);
        }
        remove_files(t.path(), &files, &linux_extra_dirs()).unwrap();
        assert!(!t.path().join(".local/share/nautilus/scripts/Archiver").exists());
        assert!(!t.path().join(".local/share/applications/archiver.desktop").exists());
    }

    #[test]
    fn macos_quick_actions() {
        let files = macos_files("/Applications/Archiver.app/Contents/MacOS/archiver");
        assert_eq!(files.len(), COMMANDS.len() * 2);
        let info = files.iter().find(|f| f.0.ends_with("Archiver - Extract here.workflow/Contents/Info.plist")).unwrap();
        assert!(info.1.contains("runWorkflowAsService") && info.1.contains("public.zip-archive"));
        let add = files.iter().find(|f| f.0.ends_with("Archiver - Add to archive.workflow/Contents/Info.plist")).unwrap();
        assert!(add.1.contains("public.item"));
        let wf = files.iter().find(|f| f.0.ends_with("Archiver - Extract to folder.workflow/Contents/document.wflow")).unwrap();
        assert!(wf.1.contains("'/Applications/Archiver.app/Contents/MacOS/archiver' --extract-to-folder \"$@\""));
        assert!(wf.1.contains("com.apple.Automator.servicesMenu"));
        let t = tempfile::tempdir().unwrap();
        write_files(t.path(), &files).unwrap();
        remove_files(t.path(), &files, &macos_extra_dirs()).unwrap();
        assert!(!t.path().join("Library/Services/Archiver - Open with Archiver.workflow").exists());
    }

    #[test]
    fn legacy_paths() {
        let w = legacy_windows_roots(WIN_BASE);
        assert!(w.contains(&r"Software\Classes\Directory\shell\RustArchiver".to_string()));
        assert!(w.contains(&r"Software\Classes\SystemFileAssociations\.zip\shell\RustArchiver".to_string()));
        assert!(!w.iter().any(|k| k.ends_with(r"\Archiver")));
        // new names must not collide with legacy ones
        let new: Vec<PathBuf> = linux_files("x").into_iter().map(|f| f.0).collect();
        assert!(legacy_linux_files().iter().all(|l| !new.contains(l)));
        assert!(legacy_dirs().contains(&PathBuf::from("Library/Services/Rust Archiver - Open with Rust Archiver.workflow")));
    }

    #[test]
    fn legacy_cleanup_removes_v020_files_only() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path();
        // simulate a v0.2.0 install
        let old: Vec<GenFile> = legacy_linux_files().into_iter().map(|p| (p, "old".into(), false)).collect();
        write_files(h, &old).unwrap();
        for d in legacy_dirs() {
            std::fs::create_dir_all(h.join(&d).join("Contents")).unwrap();
            std::fs::write(h.join(&d).join("Contents/Info.plist"), "x").unwrap();
        }
        // plus a current install
        write_files(h, &linux_files("/x/archiver")).unwrap();
        write_files(h, &macos_files("/x/archiver")).unwrap();
        let n = remove_legacy_files(h);
        assert_eq!(n, legacy_linux_files().len() + legacy_dirs().len());
        for p in legacy_linux_files() {
            assert!(!h.join(p).exists());
        }
        assert!(!h.join(".local/share/nautilus/scripts/Rust Archiver").exists());
        assert!(!h.join("Library/Services/Rust Archiver - Extract here.workflow").exists());
        assert!(h.join(".local/share/applications/archiver.desktop").exists());
        assert!(h.join(".local/share/nautilus/scripts/Archiver/Extract here").exists());
        assert!(h.join("Library/Services/Archiver - Extract here.workflow/Contents/Info.plist").exists());
        assert_eq!(remove_legacy_files(h), 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_legacy_cleanup_in_test_hive() {
        let base = r"Software\ArchiverLegacyTest\Classes";
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        for r in legacy_windows_roots(base) {
            hkcu.create_subkey(&r).unwrap();
        }
        win::apply(r"C:\x\archiver.exe", base).unwrap();
        assert_eq!(win::remove_roots(&legacy_windows_roots(base)), legacy_windows_roots(base).len());
        assert!(win::exists(base), "new menu must survive legacy cleanup");
        assert!(hkcu.open_subkey(format!(r"{base}\Directory\shell\RustArchiver")).is_err());
        win::remove(base).unwrap();
        let _ = hkcu.delete_subkey_all(r"Software\ArchiverLegacyTest");
    }

    #[cfg(windows)]
    #[test]
    fn windows_registry_roundtrip_in_test_hive() {
        let base = r"Software\ArchiverTest\Classes";
        win::apply(r"C:\x\archiver.exe", base).unwrap();
        assert!(win::exists(base));
        let v = win::read_default(&format!(r"{base}\SystemFileAssociations\.zip\shell\Archiver\shell\02extracthere\command")).unwrap();
        assert_eq!(v, r#""C:\x\archiver.exe" --extract-here "%1""#);
        win::remove(base).unwrap();
        assert!(!win::exists(base));
    }
}
