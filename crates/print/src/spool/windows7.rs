//! The experimental Windows 7 build's spooler (docs/windows7.md). craft-print draws jobs with
//! `Windows.Data.Pdf`, a Windows Runtime API Windows 7 doesn't have, so this build hands the
//! print-ready PDF to the installed PDF viewer's Print / PrintTo verb instead, as VectorCraft's
//! Windows 7 build does. The printer list still comes from the spooler (EnumPrinters, through
//! craft-print's Win32 `rgb` module). The viewer prints with its own settings: copies, two-sided
//! and scaling chosen here don't reach it, and the result says so.

use super::Job;
#[cfg(target_vendor = "win7")]
use super::Printer;
#[cfg(target_vendor = "win7")]
use crate::PrintError;

/// Where a job's PDF waits for the viewer: a fresh file in `dir`. The viewer reads it after the
/// verb returns, so it can't be removed then; files of jobs more than an hour old are removed
/// first instead. Never truncates an existing file, and removes one it couldn't finish.
pub(crate) fn job_file(dir: &std::path::Path, pdf: &[u8]) -> Result<std::path::PathBuf, String> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
    // A folder of a few files; the cap only bounds a runaway listing.
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten().take(4096) {
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 3600);
        if old {
            // Another job's viewer may still be reading it: it goes next time.
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let path = dir.join(format!("job-{}-{stamp}.pdf", std::process::id()));
    let mut file =
        std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("can't create {}: {e}", path.display()))?;
    if let Err(e) = file.write_all(pdf).and_then(|()| file.flush()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("can't write {}: {e}", path.display()));
    }
    Ok(path)
}

/// `s` as a PowerShell single-quoted string.
pub(crate) fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The PowerShell that prints `file` on `printer` (the default printer when `None`) through the
/// PDF viewer's verb. PrintTo takes the printer name in double quotes, which a Windows printer
/// name can't contain.
pub(crate) fn print_script(file: &str, printer: Option<&str>) -> String {
    let path = ps_quote(file);
    match printer.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            format!(
                "Start-Process -FilePath {path} -Verb PrintTo -ArgumentList {} -WindowStyle Hidden",
                ps_quote(&format!("\"{}\"", p.replace('"', "")))
            )
        }
        None => format!("Start-Process -FilePath {path} -Verb Print -WindowStyle Hidden"),
    }
}

/// What the dialog is told once the viewer has the job: the settings the viewer decides.
pub(crate) fn handed_over(job: &Job) -> String {
    let mut note = String::from("handed to the PDF viewer's print command, which prints with its own settings");
    if job.copies > 1 {
        note.push_str(&format!(" (one copy, not {})", job.copies));
    }
    note
}

#[cfg(target_vendor = "win7")]
fn powershell(script: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
    let mut c = std::process::Command::new(root.join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe"));
    c.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &format!("$ErrorActionPreference = 'Stop'; [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false); {script}"),
    ])
    .creation_flags(CREATE_NO_WINDOW);
    c
}

/// The spooler's printers, the default first; empty when it can't be asked.
#[cfg(target_vendor = "win7")]
pub fn printers() -> Vec<Printer> {
    let list = craft_print::rgb::printers().unwrap_or_default();
    list.into_iter().map(|p| Printer { name: p.name, description: String::new(), default: p.default, two_sided: None }).collect()
}

/// Print `pdf` through the PDF viewer's verb. Progress is reported once, when the viewer has it.
#[cfg(target_vendor = "win7")]
pub fn submit_with_progress(pdf: &[u8], job: &Job, progress: &mut dyn FnMut(usize, usize)) -> Result<String, PrintError> {
    let file = job_file(&std::env::temp_dir().join("pdfcraft-print"), pdf).map_err(PrintError::Spool)?;
    let script = print_script(&file.to_string_lossy(), job.printer.as_deref());
    let out = powershell(&script).output();
    let failed = match out {
        Ok(o) if o.status.success() => None,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            Some(if err.is_empty() { format!("PowerShell failed ({})", o.status) } else { err })
        }
        Err(e) => Some(format!("can't run PowerShell: {e}")),
    };
    if let Some(e) = failed {
        // No viewer took the job, so nothing will read the file.
        let _ = std::fs::remove_file(&file);
        return Err(PrintError::Spool(format!(
            "no app prints PDF files here; install a PDF viewer that can print, or save the print-ready PDF ({e})"
        )));
    }
    progress(1, 1);
    Ok(handed_over(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_names_and_paths_are_quoted_for_powershell() {
        assert_eq!(ps_quote("it's.pdf"), "'it''s.pdf'");
        assert_eq!(print_script(r"C:\T\job.pdf", None), r"Start-Process -FilePath 'C:\T\job.pdf' -Verb Print -WindowStyle Hidden");
        assert_eq!(print_script(r"C:\T\job.pdf", Some("  ")), r"Start-Process -FilePath 'C:\T\job.pdf' -Verb Print -WindowStyle Hidden");
        assert_eq!(
            print_script(r"C:\Bob's\job.pdf", Some("Office \"Laser\" 'B'")),
            r#"Start-Process -FilePath 'C:\Bob''s\job.pdf' -Verb PrintTo -ArgumentList '"Office Laser ''B''"' -WindowStyle Hidden"#
        );
    }

    #[test]
    fn job_files_hold_the_pdf_for_the_viewer_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-print-test-{}", std::process::id()));
        let a = job_file(&dir, b"%PDF-1.7 a").unwrap();
        let b = job_file(&dir, b"%PDF-1.7 b").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"%PDF-1.7 a");
        assert_eq!(std::fs::read(&b).unwrap(), b"%PDF-1.7 b");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_result_says_the_viewer_decides_and_copies_are_not_applied() {
        let one = Job::default();
        assert!(!handed_over(&one).contains("copy"));
        let three = Job { copies: 3, ..Job::default() };
        assert!(handed_over(&three).ends_with("(one copy, not 3)"), "{}", handed_over(&three));
    }
}
