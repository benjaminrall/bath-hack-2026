use std::env;
use std::fs;

// ─── Data Structures ────────────────────────────────────────────────────────

#[derive(Debug)]
struct Issue {
    number: u64,
    title: String,
    html_url: String,
    body: Option<String>,
    thumbs_up: u64,
    state: String,
    labels: Vec<String>,
}

// ─── GitHub API (no external crates – pure std + one optional dep) ───────────
//
// To keep this dependency-free we use `std::process::Command` to shell out to
// `curl`. If you'd rather use `reqwest`, see the commented Cargo.toml below.
//
// Add to Cargo.toml:
// [dependencies]
// serde_json = "1"          ← only dep needed
//
// The GitHub token is read from the GITHUB_TOKEN env-var (optional for public
// repos, but recommended to avoid rate-limits).

fn gh_get(url: &str, token: &Option<String>) -> Result<String, String> {
    let mut args = vec![
        "-s".to_string(),
        "-H".to_string(),
        "Accept: application/vnd.github+json".to_string(),
        "-H".to_string(),
        "X-GitHub-Api-Version: 2022-11-28".to_string(),
    ];

    if let Some(t) = token {
        args.push("-H".to_string());
        args.push(format!("Authorization: Bearer {}", t));
    }

    args.push(url.to_string());

    let output = std::process::Command::new("curl")
        .args(&args)
        .output()
        .map_err(|e| format!("curl failed: {}", e))?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ─── Minimal JSON helpers (no serde_json needed for simple cases) ────────────

fn json_str(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":", key);
    let start = json.find(&needle)? + needle.len();
    let rest = json[start..].trim_start();
    if rest.starts_with('"') {
        let inner = &rest[1..];
        let end = find_unescaped_quote(inner)?;
        Some(inner[..end].replace("\\\"", "\"").replace("\\n", "\n").replace("\\t", "\t"))
    } else if rest.starts_with("null") {
        None
    } else {
        // number / bool
        let end = rest.find(|c: char| c == ',' || c == '}' || c == ']').unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

fn find_unescaped_quote(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' { i += 2; continue; }
        if bytes[i] == b'"' { return Some(i); }
        i += 1;
    }
    None
}

fn json_number(json: &str, key: &str) -> u64 {
    json_str(json, key)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

// Split a JSON array into its top-level objects (handles nested braces)
fn split_json_array(array_str: &str) -> Vec<String> {
    let mut items = Vec::new();
    let trimmed = array_str.trim();
    if !trimmed.starts_with('[') { return items; }
    let inner = &trimmed[1..];
    let mut depth = 0i32;
    let mut start = None;
    let mut in_str = false;
    let mut prev_backslash = false;
    for (i, c) in inner.char_indices() {
        if in_str {
            if prev_backslash { prev_backslash = false; continue; }
            if c == '\\' { prev_backslash = true; continue; }
            if c == '"' { in_str = false; }
            continue;
        }
        match c {
            '"' => { in_str = true; }
            '{' => { depth += 1; if depth == 1 { start = Some(i); } }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        items.push(format!("{{{}}}", &inner[s+1..i]));
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }
    items
}

// ─── Fetch issues ────────────────────────────────────────────────────────────

fn fetch_issues(repo: &str, token: &Option<String>) -> Vec<Issue> {
    let url = format!(
        "https://api.github.com/repos/{}/issues?state=open&per_page=50&sort=created",
        repo
    );
    let raw = gh_get(&url, token).unwrap_or_default();
    let objects = split_json_array(&raw);
    let mut issues: Vec<Issue> = objects.iter().map(|obj| {
        let number  = json_number(obj, "number");
        let title   = json_str(obj, "title").unwrap_or_else(|| "Untitled".into());
        let html_url= json_str(obj, "html_url").unwrap_or_default();
        let body    = json_str(obj, "body");
        let state   = json_str(obj, "state").unwrap_or_else(|| "open".into());

        // Reactions block
        let thumbs_up = if let Some(start) = obj.find("\"reactions\"") {
            let reactions_slice = &obj[start..];
            let end = reactions_slice.find('}').map(|e| e + 1).unwrap_or(reactions_slice.len());
            json_number(&reactions_slice[..end], "+1")
        } else {
            0
        };

        // Labels array – quick extraction
        let mut labels = Vec::new();
        if let Some(li) = obj.find("\"labels\"") {
            let slice = &obj[li..];
            if let Some(arr_start) = slice.find('[') {
                let arr = &slice[arr_start..];
                let mut depth = 0i32;
                let mut label_obj_start = None;
                let mut in_s = false;
                let mut pb = false;
                for (i, c) in arr.char_indices() {
                    if in_s {
                        if pb { pb = false; continue; }
                        if c == '\\' { pb = true; continue; }
                        if c == '"' { in_s = false; }
                        continue;
                    }
                    match c {
                        '"' => in_s = true,
                        '{' => { depth += 1; if depth == 1 { label_obj_start = Some(i); } }
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                if let Some(s) = label_obj_start {
                                    let lobj = format!("{{{}}}", &arr[s+1..i]);
                                    if let Some(name) = json_str(&lobj, "name") {
                                        labels.push(name);
                                    }
                                }
                                label_obj_start = None;
                            }
                        }
                        ']' if depth == 0 => break,
                        _ => {}
                    }
                }
            }
        }

        Issue { number, title, html_url, body, thumbs_up, state, labels }
    }).collect();

    issues.sort_by(|a, b| b.thumbs_up.cmp(&a.thumbs_up));
    issues
}

// ─── Fetch JOURNAL.md ────────────────────────────────────────────────────────

fn fetch_journal(_repo: &str, _token: &Option<String>) -> String {
    // Read from local file instead of GitHub
    fs::read_to_string("JOURNAL.md")
        .unwrap_or_else(|_| "_(JOURNAL.md not found)_".to_string())
}

// ─── GitHub version (commented out for local testing) ─────────────────────────
// fn fetch_journal_github(repo: &str, token: &Option<String>) -> String {
//     let url = format!(
//         "https://api.github.com/repos/{}/contents/JOURNAL.md",
//         repo
//     );
//     let raw = gh_get(&url, token).unwrap_or_default();
//
//     // GitHub returns base64-encoded content
//     if let Some(encoded) = json_str(&raw, "content") {
//         let clean: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
//         decode_base64(&clean)
//     } else {
//         "_(JOURNAL.md not found or inaccessible)_".to_string()
//     }
// }

fn decode_base64(s: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let lookup: Vec<u8> = {
        let mut v = vec![255u8; 256];
        for (i, &c) in CHARS.iter().enumerate() { v[c as usize] = i as u8; }
        v
    };
    let bytes: Vec<u8> = s.bytes().filter(|&b| lookup[b as usize] != 255).collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 { break; }
        let b0 = lookup[chunk[0] as usize];
        let b1 = lookup[chunk[1] as usize];
        out.push((b0 << 2) | (b1 >> 4));
        if chunk.len() > 2 && chunk[2] != b'=' {
            let b2 = lookup[chunk[2] as usize];
            out.push((b1 << 4) | (b2 >> 2));
            if chunk.len() > 3 && chunk[3] != b'=' {
                let b3 = lookup[chunk[3] as usize];
                out.push((b2 << 6) | b3);
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

// ─── Extract latest journal entry ────────────────────────────────────────────

fn latest_journal_entry(journal: &str) -> String {
    let lines: Vec<&str> = journal.lines().collect();
    println!("DEBUG: Total lines found: {}", lines.len());

    let mut found_idx = None;

    for (i, line) in lines.iter().enumerate() {
        // We use contains to be 100% sure we aren't missing it due to hidden spaces
        if line.contains("## ") {
            println!("DEBUG: Found a heading at line {}: {:?}", i, line);
            found_idx = Some(i);
        }
    }

    match found_idx {
        Some(idx) => lines[idx..].join("\n"),
        None => "No headings found".to_string(),
    }
}

// ─── Simple Markdown → HTML converter ────────────────────────────────────────

fn md_to_html(md: &str) -> String {
    let mut html = String::new();
    let mut in_code = false;
    let mut in_ul = false;

    for line in md.lines() {
        // fenced code block toggle
        if line.starts_with("```") {
            if in_code {
                html.push_str("</code></pre>\n");
                in_code = false;
            } else {
                if in_ul { html.push_str("</ul>\n"); in_ul = false; }
                html.push_str("<pre><code>");
                in_code = true;
            }
            continue;
        }
        if in_code {
            html.push_str(&html_escape(line));
            html.push('\n');
            continue;
        }

        // headings
        if line.starts_with("### ") {
            if in_ul { html.push_str("</ul>\n"); in_ul = false; }
            html.push_str(&format!("<h3>{}</h3>\n", inline_md(&line[4..])));
        } else if line.starts_with("## ") {
            if in_ul { html.push_str("</ul>\n"); in_ul = false; }
            html.push_str(&format!("<h2>{}</h2>\n", inline_md(&line[3..])));
        } else if line.starts_with("# ") {
            if in_ul { html.push_str("</ul>\n"); in_ul = false; }
            html.push_str(&format!("<h1>{}</h1>\n", inline_md(&line[2..])));
        } else if line.starts_with("- ") || line.starts_with("* ") {
            if !in_ul { html.push_str("<ul>\n"); in_ul = true; }
            html.push_str(&format!("<li>{}</li>\n", inline_md(&line[2..])));
        } else if line.trim().is_empty() {
            if in_ul { html.push_str("</ul>\n"); in_ul = false; }
            html.push_str("<br>\n");
        } else {
            if in_ul { html.push_str("</ul>\n"); in_ul = false; }
            html.push_str(&format!("<p>{}</p>\n", inline_md(line)));
        }
    }
    if in_ul { html.push_str("</ul>\n"); }
    html
}

fn inline_md(s: &str) -> String {
    // bold, italic, inline code, links
    let s = html_escape(s);
    // **bold**
    let s = replace_pair(&s, "**", "<strong>", "</strong>");
    // *italic*
    let s = replace_pair(&s, "*", "<em>", "</em>");
    // `code`
    let s = replace_pair(&s, "`", "<code>", "</code>");
    s
}

fn replace_pair(s: &str, delim: &str, open: &str, close: &str) -> String {
    let mut result = String::new();
    let mut parts = s.split(delim);
    let mut toggle = false;
    while let Some(part) = parts.next() {
        result.push_str(part);
        if parts.clone().next().is_some() {
            result.push_str(if toggle { close } else { open });
            toggle = !toggle;
        }
    }
    result
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}

// ─── HTML generation ─────────────────────────────────────────────────────────

fn generate_html(journal_entry: &str, issues: &[Issue], qr_placeholder: bool) -> String {
    let journal_html = md_to_html(journal_entry);

    let issues_html: String = if issues.is_empty() {
        "<p class=\"empty\">No open issues found.</p>".to_string()
    } else {
        issues.iter().map(|issue| {
            let labels_html: String = issue.labels.iter().map(|l| {
                format!("<span class=\"label\">{}</span>", html_escape(l))
            }).collect::<Vec<_>>().join("");

            let thumbs = if issue.thumbs_up > 0 {
                format!("<span class=\"thumbs\">👍 {}</span>", issue.thumbs_up)
            } else {
                String::new()
            };

            let snippet = issue.body.as_deref().unwrap_or("").lines()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            let snippet_html = if snippet.is_empty() { String::new() } else {
                format!("<p class=\"snippet\">{}</p>", html_escape(&snippet[..snippet.len().min(120)]))
            };

            format!(r#"<a class="issue-card" href="{url}" target="_blank">
  <div class="issue-header">
    <span class="issue-num">#{num}</span>
    {thumbs}
  </div>
  <h3 class="issue-title">{title}</h3>
  {snippet}
  <div class="issue-meta">{labels}</div>
</a>"#,
                url    = html_escape(&issue.html_url),
                num    = issue.number,
                title  = html_escape(&issue.title),
                thumbs = thumbs,
                snippet = snippet_html,
                labels = labels_html,
            )
        }).collect()
    };

    let qr_section = if qr_placeholder {
        r#"<div class="qr-placeholder"><span>QR Code</span></div>"#.to_string()
    } else {
        r#"<img id="qr-img" src="qr.png" alt="QR Code">"#.to_string()
    };

    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>BATSTONE AI — Dashboard</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=Space+Mono:ital,wght@0,400;0,700;1,400&family=Syne:wght@400;700;800&display=swap" rel="stylesheet">
<style>
  :root {{
    --bg:        #0a0a0f;
    --surface:   #111118;
    --border:    #1e1e2e;
    --accent:    #7cffd4;
    --accent2:   #ff6b6b;
    --text:      #e8e8f0;
    --muted:     #666680;
    --radius:    12px;
  }}

  *, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}

  body {{
    background: var(--bg);
    color: var(--text);
    font-family: 'Space Mono', monospace;
    font-size: 14px;
    line-height: 1.7;
    min-height: 100vh;
    padding: 24px;
  }}

  /* ── Scanline grain overlay ── */
  body::before {{
    content: '';
    position: fixed;
    inset: 0;
    background-image: repeating-linear-gradient(
      0deg,
      transparent,
      transparent 2px,
      rgba(0,0,0,.08) 2px,
      rgba(0,0,0,.08) 4px
    );
    pointer-events: none;
    z-index: 9999;
  }}

  /* ── Layout ── */
  .grid {{
    display: grid;
    grid-template-columns: 180px 1fr;
    grid-template-rows: auto auto;
    gap: 16px;
    max-width: 1100px;
    margin: 0 auto;
  }}

  /* ── Header row ── */
  .qr-cell {{
    grid-column: 1;
    grid-row: 1;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    min-height: 180px;
  }}

  .qr-placeholder {{
    width: 140px;
    height: 140px;
    border: 2px dashed var(--muted);
    border-radius: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: 12px;
    letter-spacing: 1px;
  }}

  #qr-img {{ width: 140px; height: 140px; object-fit: contain; border-radius: 8px; }}

  .title-cell {{
    grid-column: 2;
    grid-row: 1;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    justify-content: center;
    padding: 28px 36px;
    position: relative;
    overflow: hidden;
  }}

  .title-cell::after {{
    content: '';
    position: absolute;
    top: -40%;
    right: -10%;
    width: 300px;
    height: 300px;
    background: radial-gradient(circle, rgba(124,255,212,.07) 0%, transparent 70%);
    pointer-events: none;
  }}

  .title-cell h1 {{
    font-family: 'Syne', sans-serif;
    font-weight: 800;
    font-size: clamp(28px, 4vw, 48px);
    letter-spacing: -1px;
    color: var(--accent);
    line-height: 1;
  }}

  .title-cell .subtitle {{
    color: var(--muted);
    font-size: 11px;
    letter-spacing: 3px;
    text-transform: uppercase;
    margin-top: 8px;
  }}

  /* ── Journal ── */
  .journal-cell {{
    grid-column: 1 / -1;
    grid-row: 2;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 28px 32px;
  }}

  .section-label {{
    font-size: 10px;
    letter-spacing: 3px;
    text-transform: uppercase;
    color: var(--accent);
    margin-bottom: 14px;
    display: flex;
    align-items: center;
    gap: 8px;
  }}

  .section-label::after {{
    content: '';
    flex: 1;
    height: 1px;
    background: var(--border);
  }}

  .journal-cell h1, .journal-cell h2 {{
    font-family: 'Syne', sans-serif;
    font-weight: 700;
    color: var(--accent);
    margin-bottom: 8px;
    margin-top: 16px;
  }}
  .journal-cell h3 {{ color: var(--text); margin: 12px 0 6px; }}
  .journal-cell p  {{ color: #b0b0c8; margin-bottom: 6px; }}
  .journal-cell ul {{ padding-left: 20px; color: #b0b0c8; margin-bottom: 8px; }}
  .journal-cell li {{ margin-bottom: 2px; }}
  .journal-cell code {{
    background: #1a1a2e;
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 12px;
    color: var(--accent2);
  }}
  .journal-cell pre {{
    background: #0d0d1a;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 14px;
    overflow-x: auto;
    margin: 10px 0;
  }}
  .journal-cell pre code {{ background: none; padding: 0; color: var(--text); }}

  /* ── Issues ── */
  .issues-cell {{
    grid-column: 1 / -1;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 28px 32px;
  }}

  .issues-grid {{
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 12px;
    margin-top: 4px;
  }}

  .issue-card {{
    display: block;
    background: #0d0d18;
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 16px;
    text-decoration: none;
    color: var(--text);
    transition: border-color .2s, transform .15s;
    position: relative;
    overflow: hidden;
  }}

  .issue-card:hover {{
    border-color: var(--accent);
    transform: translateY(-2px);
  }}

  .issue-card:hover::before {{
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(135deg, rgba(124,255,212,.04), transparent);
    pointer-events: none;
  }}

  .issue-header {{
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 6px;
  }}

  .issue-num {{ color: var(--muted); font-size: 11px; }}

  .thumbs {{
    font-size: 11px;
    background: rgba(124,255,212,.1);
    color: var(--accent);
    padding: 2px 8px;
    border-radius: 20px;
  }}

  .issue-title {{
    font-family: 'Syne', sans-serif;
    font-weight: 700;
    font-size: 13px;
    margin-bottom: 6px;
    color: var(--text);
  }}

  .snippet {{ font-size: 11px; color: var(--muted); margin-bottom: 8px; }}

  .issue-meta {{ display: flex; flex-wrap: wrap; gap: 4px; }}

  .label {{
    font-size: 10px;
    padding: 2px 7px;
    border-radius: 20px;
    background: rgba(255,107,107,.12);
    color: var(--accent2);
    letter-spacing: .5px;
  }}

  .empty {{ color: var(--muted); font-style: italic; }}

  /* ── Responsive ── */
  @media (max-width: 600px) {{
    .grid {{ grid-template-columns: 1fr; }}
    .qr-cell {{ min-height: 120px; }}
  }}
</style>
</head>
<body>
<div class="grid">

  <!-- QR + Title -->
  <div class="qr-cell">
    {qr}
  </div>

  <div class="title-cell">
    <h1>BATSTONE AI</h1>
    <p class="subtitle">Bath Hack 2026 · Live Dashboard</p>
  </div>

  <!-- Journal -->
  <div class="journal-cell">
    <div class="section-label">Latest Journal Entry</div>
    {journal}
  </div>

  <!-- Issues -->
  <div class="issues-cell">
    <div class="section-label">Open Issues · sorted by 👍</div>
    <div class="issues-grid">
      {issues}
    </div>
  </div>

</div>
</body>
</html>"#,
        qr      = qr_section,
        journal = journal_html,
        issues  = issues_html,
    )
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    let repo  = "benjaminrall/bath-hack-2026";
    let token = env::var("GITHUB_TOKEN").ok();

    eprintln!("🔍  Fetching JOURNAL.md …");
    let journal_raw   = fetch_journal(repo, &token);
    let journal_entry = latest_journal_entry(&journal_raw);

    eprintln!("🔍  Fetching issues …");
    let issues = fetch_issues(repo, &token);
    eprintln!("    {} issues found", issues.len());

    let html = generate_html(&journal_entry, &issues, true);

    let out = "dashboard.html";
    fs::write(out, &html).expect("Failed to write dashboard.html");
    eprintln!("✅  Written to {}", out);

    // Optionally open in browser (macOS / Linux)
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(out).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(out).spawn();
}