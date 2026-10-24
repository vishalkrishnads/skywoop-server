//! Video prompting guide shipped in the API binary.
//!
//! Agents read slices via outline / section / search / all. The file is never
//! sent to the client except as the text of a `read_video_guide` tool response.

const SOURCE: &str = include_str!("../prompts/video_guide.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoGuidePart {
    Outline,
    Section,
    Search,
    All,
}

impl VideoGuidePart {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "outline" | "" => Some(Self::Outline),
            "section" => Some(Self::Section),
            "search" => Some(Self::Search),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Heading {
    /// 1 = `#`, 2 = `##`, 3 = `###`
    level: usize,
    title: String,
    /// 0-based line index of the heading.
    start: usize,
    /// Exclusive end line (start of next same-or-higher heading, or EOF).
    end: usize,
}

/// Full video guide markdown (compile-time include).
pub fn source() -> &'static str {
    SOURCE
}

/// Read a slice of the video guide. Returns plain text for the agent or API.
pub fn read(
    part: VideoGuidePart,
    heading: Option<&str>,
    query: Option<&str>,
) -> Result<String, String> {
    let lines: Vec<&str> = SOURCE.lines().collect();
    let headings = parse_headings(&lines);

    match part {
        VideoGuidePart::All => Ok(SOURCE.to_owned()),
        VideoGuidePart::Outline => Ok(outline_text(&headings)),
        VideoGuidePart::Section => {
            let want = heading
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .ok_or_else(|| {
                    "section needs a heading (e.g. \"Seedance\" or \"The short version\")."
                        .to_owned()
                })?;
            let h = find_heading(&headings, want).ok_or_else(|| {
                format!(
                    "unknown heading \"{want}\". Known: {}",
                    headings
                        .iter()
                        .map(|x| x.title.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            Ok(section_text(&lines, h))
        }
        VideoGuidePart::Search => {
            let q = query
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or_else(|| "search needs a query.".to_owned())?
                .to_ascii_lowercase();
            let hits: Vec<&Heading> = headings
                .iter()
                .filter(|h| {
                    let body = lines[h.start..h.end].join("\n").to_ascii_lowercase();
                    h.title.to_ascii_lowercase().contains(&q) || body.contains(&q)
                })
                .collect();
            if hits.is_empty() {
                return Ok(format!("No video guide sections matched \"{q}\"."));
            }
            let mut out = String::new();
            for (i, h) in hits.iter().enumerate() {
                if i > 0 {
                    out.push_str("\n\n---\n\n");
                }
                out.push_str(&section_text(&lines, h));
            }
            Ok(out)
        }
    }
}

fn parse_headings(lines: &[&str]) -> Vec<Heading> {
    let mut starts: Vec<(usize, usize, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let bare = line.trim_end_matches('\r');
        if let Some((level, title)) = heading_line(bare) {
            starts.push((i, level, title));
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (idx, (start, level, title)) in starts.iter().enumerate() {
        let end = starts
            .iter()
            .skip(idx + 1)
            .find(|(_, lvl, _)| *lvl <= *level)
            .map(|(i, _, _)| *i)
            .unwrap_or(lines.len());
        out.push(Heading {
            level: *level,
            title: title.clone(),
            start: *start,
            end,
        });
    }
    out
}

fn heading_line(line: &str) -> Option<(usize, String)> {
    let bytes = line.as_bytes();
    if bytes.is_empty() || bytes[0] != b'#' {
        return None;
    }
    let mut level = 0usize;
    while level < bytes.len() && bytes[level] == b'#' {
        level += 1;
    }
    if level == 0 || level > 3 {
        return None;
    }
    if level < bytes.len() && bytes[level] != b' ' {
        return None;
    }
    let title = line[level..].trim().to_owned();
    if title.is_empty() {
        return None;
    }
    Some((level, title))
}

fn find_heading<'a>(headings: &'a [Heading], want: &str) -> Option<&'a Heading> {
    let want_l = want.to_ascii_lowercase();
    headings
        .iter()
        .find(|h| h.title.eq_ignore_ascii_case(want))
        .or_else(|| {
            headings
                .iter()
                .find(|h| h.title.to_ascii_lowercase().contains(&want_l))
        })
}

fn outline_text(headings: &[Heading]) -> String {
    if headings.is_empty() {
        return "(empty video guide)".into();
    }
    headings
        .iter()
        .map(|h| format!("{} {}", "#".repeat(h.level), h.title))
        .collect::<Vec<_>>()
        .join("\n")
}

fn section_text(lines: &[&str], h: &Heading) -> String {
    lines[h.start..h.end].join("\n").trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_lists_headings() {
        let text = read(VideoGuidePart::Outline, None, None).unwrap();
        assert!(text.contains("## The short version"));
        assert!(text.contains("## Seedance"));
        assert!(text.contains("## Kling"));
        assert!(!text.contains("What stays the same"));
    }

    #[test]
    fn section_by_seedance_heading() {
        let text = read(VideoGuidePart::Section, Some("Seedance"), None).unwrap();
        assert!(text.contains("## Seedance"));
        assert!(text.contains("give each reference a job"));
        assert!(text.contains("@Image1"));
    }

    #[test]
    fn unknown_heading_lists_known() {
        let err = read(VideoGuidePart::Section, Some("DoesNotExist"), None).unwrap_err();
        assert!(err.contains("unknown heading"));
        assert!(err.contains("Seedance"));
    }

    #[test]
    fn all_is_full_file() {
        let text = read(VideoGuidePart::All, None, None).unwrap();
        assert_eq!(text, SOURCE);
    }
}
