//! Archive contents report (WinRAR "Generate report", Alt+G): plain text, CSV or HTML.

use std::path::Path;

use archiver_core::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Text,
    Csv,
    Html,
}

impl ReportFormat {
    /// From the file extension of the chosen output path (`.csv`, `.htm`/`.html`; anything else is text).
    pub fn from_path(p: &Path) -> ReportFormat {
        match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("csv") => ReportFormat::Csv,
            Some("htm" | "html") => ReportFormat::Html,
            _ => ReportFormat::Text,
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            ReportFormat::Text => "txt",
            ReportFormat::Csv => "csv",
            ReportFormat::Html => "html",
        }
    }
}

/// Column titles shared by the text and HTML reports.
const HEAD: [&str; 6] = ["名稱", "大小", "壓縮後", "修改時間", "CRC32", "加密"];
/// Numeric columns (size, compressed) are right-aligned in the text report.
const RIGHT: [bool; 6] = [false, true, true, false, false, false];

/// The report of `archive` (`fmt_label`: e.g. "ZIP") listing `entries` in archive order.
pub fn render(archive: &Path, fmt_label: &str, entries: &[Entry], format: ReportFormat) -> String {
    match format {
        ReportFormat::Text => text(archive, fmt_label, entries),
        ReportFormat::Csv => csv(entries),
        ReportFormat::Html => html(archive, fmt_label, entries),
    }
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Display cells per entry, in [`HEAD`] order.
fn cells(entries: &[Entry]) -> Vec<[String; 6]> {
    entries
        .iter()
        .map(|e| {
            [
                e.name.clone(),
                if e.is_dir { "<資料夾>".to_string() } else { e.size.to_string() },
                e.compressed.map(|c| c.to_string()).unwrap_or_default(),
                e.modified.clone().unwrap_or_default(),
                e.crc32.map(|c| format!("{c:08X}")).unwrap_or_default(),
                if e.encrypted { "*".to_string() } else { String::new() },
            ]
        })
        .collect()
}

/// Totals line: file and folder counts, uncompressed and compressed byte totals.
fn totals_line(entries: &[Entry]) -> String {
    let files = entries.iter().filter(|e| !e.is_dir).count();
    let dirs = entries.iter().filter(|e| e.is_dir).count();
    let size: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let comp: u64 = entries.iter().filter(|e| !e.is_dir).filter_map(|e| e.compressed).sum();
    format!("總共 {files} 個檔案、{dirs} 個資料夾，{size} 個位元組（壓縮後 {comp} 個位元組）")
}

/// Pad by character count (CJK columns are not exactly aligned; good enough for a report).
fn pad(s: &str, w: usize, right: bool) -> String {
    let fill = " ".repeat(w.saturating_sub(s.chars().count()));
    if right { format!("{fill}{s}") } else { format!("{s}{fill}") }
}

fn table_line<S: AsRef<str>>(cols: &[S], w: &[usize; 6]) -> String {
    let parts: Vec<String> = cols.iter().enumerate().map(|(i, c)| pad(c.as_ref(), w[i], RIGHT[i])).collect();
    parts.join("  ").trim_end().to_string()
}

fn text(archive: &Path, fmt_label: &str, entries: &[Entry]) -> String {
    let rows = cells(entries);
    let mut w = HEAD.map(|h| h.chars().count());
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let rule = "-".repeat(w.iter().sum::<usize>() + 2 * (w.len() - 1));
    let mut out = String::new();
    out.push_str(&format!("壓縮檔：{}\r\n", archive.display()));
    out.push_str(&format!("格式：{fmt_label}\r\n"));
    out.push_str(&format!("產生時間：{}\r\n\r\n", now()));
    out.push_str(&table_line(&HEAD, &w));
    out.push_str("\r\n");
    out.push_str(&rule);
    out.push_str("\r\n");
    for r in &rows {
        out.push_str(&table_line(r, &w));
        out.push_str("\r\n");
    }
    out.push_str(&rule);
    out.push_str("\r\n");
    out.push_str(&totals_line(entries));
    out.push_str("\r\n");
    out
}

/// RFC 4180: quote when the field holds a comma, double quote or line break; double inner quotes.
/// A field Excel would read as a formula (leading `= + - @`, tab or CR) gets a `'` prefix first (OWASP).
fn csv_field(s: &str) -> String {
    let guarded;
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        guarded = format!("'{s}");
        guarded.as_str()
    } else {
        s
    };
    if s.contains([',', '"', '\r', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

fn csv(entries: &[Entry]) -> String {
    // UTF-8 BOM so Excel detects the encoding.
    let mut out = String::from("\u{feff}name,size,compressed,modified,is_dir,crc32,encrypted\r\n");
    for e in entries {
        let fields = [
            e.name.clone(),
            e.size.to_string(),
            e.compressed.map(|c| c.to_string()).unwrap_or_default(),
            e.modified.clone().unwrap_or_default(),
            e.is_dir.to_string(),
            e.crc32.map(|c| format!("{c:08X}")).unwrap_or_default(),
            e.encrypted.to_string(),
        ];
        out.push_str(&fields.iter().map(|f| csv_field(f.as_str())).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

/// Escape the five HTML-significant characters.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

const HTML_CSS: &str = "body{font-family:\"Microsoft JhengHei\",\"Segoe UI\",sans-serif;margin:16px;color:#222;background:#fff}\n\
h1{font-size:1.3em}\n\
table{border-collapse:collapse;width:100%}\n\
th,td{border:1px solid #bbb;padding:4px 8px;text-align:left;vertical-align:top}\n\
th{background:#eee}\n\
td.n{text-align:right;white-space:nowrap}\n\
tfoot td{font-weight:bold;background:#f6f6f6}\n";

fn html(archive: &Path, fmt_label: &str, entries: &[Entry]) -> String {
    let title = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| archive.display().to_string());
    let mut o = String::new();
    o.push_str("<!DOCTYPE html>\n<html lang=\"zh-Hant\">\n<head>\n<meta charset=\"utf-8\">\n");
    o.push_str(&format!("<title>{}</title>\n<style>\n{HTML_CSS}</style>\n</head>\n<body>\n", esc(&title)));
    o.push_str(&format!("<h1>{}</h1>\n", esc(&title)));
    o.push_str(&format!("<p>壓縮檔：{} ｜ 格式：{} ｜ 產生時間：{}</p>\n", esc(&archive.display().to_string()), esc(fmt_label), now()));
    o.push_str("<table>\n<thead><tr>");
    for h in HEAD {
        o.push_str(&format!("<th>{}</th>", esc(h)));
    }
    o.push_str("</tr></thead>\n<tbody>\n");
    for r in cells(entries) {
        o.push_str("<tr>");
        for (i, c) in r.iter().enumerate() {
            if RIGHT[i] {
                o.push_str(&format!("<td class=\"n\">{}</td>", esc(c)));
            } else {
                o.push_str(&format!("<td>{}</td>", esc(c)));
            }
        }
        o.push_str("</tr>\n");
    }
    o.push_str("</tbody>\n");
    o.push_str(&format!("<tfoot><tr><td colspan=\"{}\">{}</td></tr></tfoot>\n", HEAD.len(), esc(&totals_line(entries))));
    o.push_str("</table>\n</body>\n</html>\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, size: u64, is_dir: bool) -> Entry {
        Entry { name: name.to_string(), size, compressed: None, modified: None, is_dir, crc32: None, encrypted: false }
    }

    #[test]
    fn csv_neutralises_formula_prefixes() {
        for n in ["=1+1", "+cmd", "-2", "@SUM(A1)", "\tx"] {
            assert_eq!(csv_field(n), format!("'{n}"), "{n:?}");
        }
        assert_eq!(csv_field("=a,b"), "\"'=a,b\"");
        assert_eq!(csv_field("\rx"), "\"'\rx\"");
        assert_eq!(csv_field("plain"), "plain");
        let out = csv(&[entry("=HYPERLINK(\"x\")", 1, false)]);
        assert!(out.contains("\r\n\"'=HYPERLINK(\"\"x\"\")\",1,"), "{out}");
    }

    fn sample() -> Vec<Entry> {
        vec![
            Entry { name: "d".into(), size: 0, compressed: None, modified: Some("2026-01-01 10:00".into()), is_dir: true, crc32: None, encrypted: false },
            Entry { name: "d/報告 一.txt".into(), size: 10, compressed: Some(4), modified: Some("2026-01-02 03:04".into()), is_dir: false, crc32: Some(0xDEADBEEF), encrypted: true },
            Entry { name: "b.bin".into(), size: 5, compressed: None, modified: None, is_dir: false, crc32: None, encrypted: false },
        ]
    }

    /// Drop the timestamp line, which changes every run.
    fn strip_time(s: &str) -> String {
        s.lines().filter(|l| !l.starts_with("產生時間")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn from_path_extensions() {
        assert_eq!(ReportFormat::from_path(Path::new("r.CSV")), ReportFormat::Csv);
        assert_eq!(ReportFormat::from_path(Path::new("r.htm")), ReportFormat::Html);
        assert_eq!(ReportFormat::from_path(Path::new("r.HTML")), ReportFormat::Html);
        assert_eq!(ReportFormat::from_path(Path::new("r.txt")), ReportFormat::Text);
        assert_eq!(ReportFormat::from_path(Path::new("r")), ReportFormat::Text);
        assert_eq!(ReportFormat::from_path(Path::new("r.csv.bak")), ReportFormat::Text);
    }

    #[test]
    fn csv_has_bom_header_crlf_and_quoting() {
        let entries = vec![
            entry("a,b.txt", 3, false),
            Entry { name: "say \"hi\".txt".into(), size: 1, compressed: Some(2), modified: Some("2026-01-02 03:04".into()), is_dir: false, crc32: Some(0xDEADBEEF), encrypted: true },
            entry("line\nbreak", 0, false),
        ];
        let out = render(Path::new("x.zip"), "ZIP", &entries, ReportFormat::Csv);
        assert!(out.starts_with("\u{feff}name,size,compressed,modified,is_dir,crc32,encrypted\r\n"));
        assert!(out.ends_with("\r\n"));
        assert!(out.contains("\"a,b.txt\",3,,,false,,false\r\n"));
        assert!(out.contains("\"say \"\"hi\"\".txt\",1,2,2026-01-02 03:04,false,DEADBEEF,true\r\n"));
        assert!(out.contains("\"line\nbreak\",0,,,false,,false\r\n"));
        // every row ends in CRLF; the only bare LF is inside the quoted "line\nbreak" field
        let unquoted = out.replace("\"line\nbreak\"", "");
        assert_eq!(unquoted.matches('\n').count(), unquoted.matches("\r\n").count());
    }

    #[test]
    fn html_escapes_values_and_has_structure() {
        let entries = vec![entry("<script>&\"'", 1, false)];
        let out = render(Path::new("<evil>&.zip"), "ZIP", &entries, ReportFormat::Html);
        assert!(out.starts_with("<!DOCTYPE html>"));
        assert!(out.contains("<meta charset=\"utf-8\">"));
        assert!(out.contains("<title>&lt;evil&gt;&amp;.zip</title>"));
        assert!(out.contains("&lt;script&gt;&amp;&quot;&#39;"));
        assert!(!out.contains("<script>"));
        assert!(out.contains("<tfoot>"));
        assert!(out.contains("</html>"));
    }

    #[test]
    fn text_has_header_columns_and_totals() {
        let out = strip_time(&render(Path::new("C:/a/x.zip"), "ZIP", &sample(), ReportFormat::Text));
        // strip_time joins lines with LF
        assert!(out.starts_with("壓縮檔：C:/a/x.zip\n格式：ZIP\n\n名稱"));
        assert!(out.contains("名稱"));
        assert!(out.contains("<資料夾>"));
        assert!(out.contains("DEADBEEF"));
        assert!(out.contains('*'));
        let last = out.lines().last().unwrap();
        assert_eq!(last, "總共 2 個檔案、1 個資料夾，15 個位元組（壓縮後 4 個位元組）");
    }

    #[test]
    fn text_timestamp_line_is_present() {
        let out = render(Path::new("x.zip"), "ZIP", &[], ReportFormat::Text);
        let line = out.lines().find(|l| l.starts_with("產生時間：")).unwrap();
        let ts = line.trim_start_matches("產生時間：");
        assert_eq!(ts.len(), "2026-01-01 00:00:00".len());
        assert!(out.ends_with("總共 0 個檔案、0 個資料夾，0 個位元組（壓縮後 0 個位元組）\r\n"));
    }

    #[test]
    fn render_is_deterministic_apart_from_time() {
        let a = strip_time(&render(Path::new("x.zip"), "ZIP", &sample(), ReportFormat::Html));
        let b = strip_time(&render(Path::new("x.zip"), "ZIP", &sample(), ReportFormat::Html));
        assert_eq!(a, b);
    }
}
