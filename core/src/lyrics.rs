use serde::Deserialize;
use yaml_rust2::parser::{Event, EventReceiver, Parser};

const MAX_LYRIC_BYTES: usize = 256_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WordCue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricLine {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub words: Option<Vec<WordCue>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LyricsQuality {
    Word,
    Mixed,
    Line,
    Estimated,
    Plain,
    Unavailable,
    Instrumental,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricsResult {
    pub lines: Vec<LyricLine>,
    pub quality: LyricsQuality,
    pub message: Option<String>,
}

#[derive(Clone, Debug)]
struct RawWord {
    start_ms: u64,
    text: String,
}

#[derive(Clone, Debug)]
struct RawCue {
    start_ms: u64,
    end_ms: Option<u64>,
    text: String,
    words: Vec<RawWord>,
}

#[derive(Clone, Copy, Debug)]
struct InlineAnchor {
    start: usize,
    end: usize,
    time_ms: u64,
}

pub fn parse_lrc(source: &str, duration_ms: Option<u64>) -> LyricsResult {
    if source.len() > MAX_LYRIC_BYTES {
        return unavailable("Lyric text exceeds the supported size.");
    }

    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let offset_ms = parse_offset(source);
    let mut cues = Vec::new();

    for line in source.lines() {
        let line = line.trim_start();
        let mut content = line;
        let mut timestamps = Vec::new();
        while let Some((timestamp, consumed)) = parse_stamp(content, b'[') {
            timestamps.push(timestamp);
            content = content[consumed..].trim_start();
        }
        if timestamps.is_empty() {
            continue;
        }

        let anchors = parse_inline_anchors(content);
        let text = remove_anchors(content, &anchors).trim().to_owned();
        for timestamp in &timestamps {
            let start_ms = apply_offset(*timestamp, offset_ms);
            let delta = *timestamp as i128 - timestamps[0] as i128 + offset_ms as i128;
            let (words, end_ms) = parse_word_cues(content, &anchors, start_ms, delta);
            cues.push(RawCue {
                start_ms,
                end_ms,
                text: text.clone(),
                words,
            });
        }
    }

    timed_result(cues, duration_ms)
}

pub fn parse_ttml(source: &str, duration_ms: Option<u64>) -> LyricsResult {
    if source.len() > MAX_LYRIC_BYTES {
        return unavailable("Lyric text exceeds the supported size.");
    }

    let document = match roxmltree::Document::parse(source) {
        Ok(document) => document,
        Err(_) => return unavailable("Invalid TTML XML."),
    };
    let root = document.root_element();
    let namespace = root.tag_name().namespace().unwrap_or("");
    if root.tag_name().name() != "tt"
        || !matches!(
            namespace,
            "" | "http://www.w3.org/ns/ttml" | "http://www.w3.org/2006/10/ttaf1"
        )
    {
        return unavailable("The XML document is not a supported TTML file.");
    }

    let paragraphs: Vec<_> = root
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().name() == "p"
                && node
                    .ancestors()
                    .any(|ancestor| ancestor.is_element() && ancestor.tag_name().name() == "body")
        })
        .collect();
    if paragraphs.is_empty() {
        return unavailable("No lyric paragraphs in the TTML file.");
    }

    let mut cues = Vec::new();
    let mut plain_lines = Vec::new();
    let mut has_untimed_paragraph = false;
    for paragraph in paragraphs {
        let text = collect_xml_text(paragraph).trim().to_owned();
        if text.is_empty() {
            continue;
        }
        plain_lines.push(text.clone());
        if paragraph.ancestors().any(|ancestor| {
            ancestor
                .attribute("timeContainer")
                .is_some_and(|container| container.eq_ignore_ascii_case("seq"))
        }) {
            has_untimed_paragraph = true;
            continue;
        }

        let (parent_offset_ms, inherited_timing) = match ttml_parent_offset(paragraph) {
            Some(offset) => offset,
            None => return unavailable("Unsupported TTML paragraph timing."),
        };
        let start_ms = match paragraph.attribute("begin") {
            Some(begin) => match parse_ttml_time(begin) {
                Some(begin_ms) => parent_offset_ms.saturating_add(begin_ms),
                None => return unavailable("Unsupported TTML begin time."),
            },
            None if inherited_timing => parent_offset_ms,
            None => {
                has_untimed_paragraph = true;
                continue;
            }
        };

        let end_ms = if let Some(end) = paragraph.attribute("end") {
            let end_ms = match parse_ttml_time(end) {
                Some(end_ms) => parent_offset_ms.saturating_add(end_ms),
                None => return unavailable("Unsupported TTML end time."),
            };
            if end_ms <= start_ms {
                return unavailable("TTML paragraph end must follow its begin time.");
            }
            Some(end_ms)
        } else if let Some(duration) = paragraph.attribute("dur") {
            let duration_ms = match parse_ttml_time(duration) {
                Some(duration_ms) if duration_ms > 0 => duration_ms,
                _ => return unavailable("Unsupported TTML paragraph duration."),
            };
            Some(start_ms.saturating_add(duration_ms))
        } else {
            None
        };

        let words = ttml_word_cues(paragraph, start_ms, &text);
        cues.push(RawCue {
            start_ms,
            end_ms,
            text,
            words,
        });
    }

    if plain_lines.is_empty() {
        return unavailable("No lyric paragraphs in the TTML file.");
    }
    if has_untimed_paragraph || cues.is_empty() {
        return parse_plain(&plain_lines.join("\n"));
    }
    timed_result(cues, duration_ms)
}

pub fn parse_plain(source: &str) -> LyricsResult {
    if source.len() > MAX_LYRIC_BYTES {
        return unavailable("Lyric text exceeds the supported size.");
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return unavailable("No lyrics in the file.");
    }

    let lines: Vec<_> = trimmed
        .lines()
        .map(|text| LyricLine {
            start_ms: 0,
            end_ms: 0,
            text: text.to_owned(),
            words: None,
        })
        .collect();
    LyricsResult {
        lines,
        quality: LyricsQuality::Plain,
        message: None,
    }
}

pub fn parse_lyricsfile(source: &str, duration_ms: Option<u64>) -> LyricsResult {
    const MAX_EVENTS: usize = 50_000;
    const MAX_NESTING: usize = 16;
    const MAX_LINES: usize = 10_000;
    const MAX_WORDS: usize = 100_000;
    const MAX_FIELD_CHARS: usize = 16_384;

    if source.len() > MAX_LYRIC_BYTES {
        return unavailable("Lyricsfile exceeds the supported size.");
    }

    #[derive(Default)]
    struct Validator {
        event_count: usize,
        document_count: usize,
        depth: usize,
        valid: bool,
    }

    impl EventReceiver for Validator {
        fn on_event(&mut self, event: Event) {
            self.event_count += 1;
            if self.event_count > MAX_EVENTS {
                self.valid = false;
                return;
            }
            match event {
                Event::DocumentStart => self.document_count += 1,
                Event::Alias(_) => self.valid = false,
                Event::Scalar(_, _, anchor, tag) => {
                    if anchor != 0 || tag.is_some() {
                        self.valid = false;
                    }
                }
                Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag) => {
                    self.depth += 1;
                    if anchor != 0 || tag.is_some() || self.depth > MAX_NESTING {
                        self.valid = false;
                    }
                }
                Event::SequenceEnd | Event::MappingEnd => {
                    self.depth = self.depth.saturating_sub(1);
                }
                _ => {}
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        version: String,
        metadata: Metadata,
        lines: Option<Vec<FileLine>>,
        plain: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Metadata {
        title: String,
        artist: String,
        album: Option<String>,
        duration_ms: Option<i64>,
        offset_ms: Option<i64>,
        language: Option<String>,
        instrumental: Option<bool>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileLine {
        text: String,
        start_ms: i64,
        end_ms: Option<i64>,
        words: Option<Vec<FileWord>>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileWord {
        text: String,
        start_ms: i64,
        end_ms: Option<i64>,
    }

    let mut validator = Validator {
        valid: true,
        ..Validator::default()
    };
    let mut parser = Parser::new_from_str(source);
    if parser.load(&mut validator, true).is_err()
        || !validator.valid
        || validator.document_count != 1
        || validator.depth != 0
    {
        return unavailable("Invalid or unsupported Lyricsfile YAML.");
    }

    let file: File = match serde_yaml_ng::from_str(source) {
        Ok(file) => file,
        Err(_) => return unavailable("Invalid Lyricsfile structure."),
    };
    if file.version != "1.0" {
        return unavailable("Unsupported Lyricsfile version.");
    }
    let metadata = file.metadata;
    if metadata.title.trim().is_empty()
        || metadata.artist.trim().is_empty()
        || metadata.title.chars().count() > 512
        || metadata.artist.chars().count() > 512
        || metadata
            .album
            .as_ref()
            .is_some_and(|value| value.chars().count() > 512)
        || metadata
            .language
            .as_ref()
            .is_some_and(|value| value.chars().count() > 64)
        || metadata.offset_ms.is_some_and(|offset| offset != 0)
    {
        return unavailable("Invalid or unsupported Lyricsfile metadata.");
    }
    let metadata_duration = match metadata.duration_ms {
        Some(duration) if duration < 0 || duration > 86_400_000 => {
            return unavailable("Invalid Lyricsfile duration.");
        }
        Some(duration) => u64::try_from(duration).ok(),
        None => None,
    };
    let effective_duration = duration_ms.or(metadata_duration);
    let file_lines = file.lines.unwrap_or_default();
    let plain = file.plain.unwrap_or_default();

    if metadata.instrumental.unwrap_or(false) {
        if file_lines.iter().any(|line| !line.text.trim().is_empty()) || !plain.trim().is_empty() {
            return unavailable("Instrumental Lyricsfiles must not contain lyrics.");
        }
        return LyricsResult {
            lines: Vec::new(),
            quality: LyricsQuality::Instrumental,
            message: None,
        };
    }
    if file_lines.len() > MAX_LINES {
        return unavailable("Lyricsfile contains too many lines.");
    }
    if file_lines.is_empty() {
        return parse_plain(&plain);
    }

    let mut total_words = 0usize;
    let mut parsed_lines = Vec::with_capacity(file_lines.len());
    for line in file_lines {
        if line.text.chars().count() > MAX_FIELD_CHARS || line.start_ms < 0 {
            return unavailable("Invalid Lyricsfile line.");
        }
        let start_ms = match u64::try_from(line.start_ms) {
            Ok(start_ms) => start_ms,
            Err(_) => return unavailable("Invalid Lyricsfile line time."),
        };
        let end_ms = match line.end_ms {
            Some(end) if end < line.start_ms => {
                return unavailable("Lyricsfile line end precedes its start.");
            }
            Some(end) => match u64::try_from(end) {
                Ok(end) => Some(end),
                Err(_) => return unavailable("Invalid Lyricsfile line end."),
            },
            None => None,
        };
        let words = line.words.unwrap_or_default();
        total_words = total_words.saturating_add(words.len());
        if total_words > MAX_WORDS {
            return unavailable("Lyricsfile contains too many word cues.");
        }
        let mut parsed_words = Vec::with_capacity(words.len());
        for word in words {
            if word.text.chars().count() > MAX_FIELD_CHARS || word.start_ms < 0 {
                return unavailable("Invalid Lyricsfile word.");
            }
            let word_start = match u64::try_from(word.start_ms) {
                Ok(start) if start >= start_ms => start,
                _ => return unavailable("Lyricsfile word starts before its line."),
            };
            let word_end = match word.end_ms {
                Some(end) if end < word.start_ms => {
                    return unavailable("Lyricsfile word end precedes its start.");
                }
                Some(end) => match u64::try_from(end) {
                    Ok(end) if end >= start_ms => Some(end),
                    _ => return unavailable("Invalid Lyricsfile word end."),
                },
                None => None,
            };
            if end_ms.is_some_and(|line_end| {
                word_start > line_end || word_end.is_some_and(|word_end| word_end > line_end)
            }) {
                return unavailable("Lyricsfile word falls outside its line.");
            }
            parsed_words.push((word_start, word_end, word.text));
        }
        if parsed_words.windows(2).any(|pair| pair[1].0 < pair[0].0) {
            return unavailable("Lyricsfile words are not ordered by start time.");
        }
        parsed_lines.push((start_ms, end_ms, line.text, parsed_words));
    }

    parsed_lines.sort_by_key(|line| line.0);
    let mut lines = Vec::with_capacity(parsed_lines.len());
    for (index, (start_ms, explicit_end, text, words)) in parsed_lines.iter().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let word_boundary = words.iter().map(|word| word.1.unwrap_or(word.0)).max();
        let inferred_end = parsed_lines
            .iter()
            .skip(index + 1)
            .find(|next| next.0 > *start_ms)
            .map(|next| next.0)
            .or_else(|| effective_duration.filter(|duration| *duration > *start_ms))
            .or(word_boundary)
            .unwrap_or_else(|| start_ms.saturating_add(4_000));
        let line_end =
            explicit_end.unwrap_or_else(|| inferred_end.max(word_boundary.unwrap_or_default()));
        let joined_words: String = words.iter().map(|word| word.2.as_str()).collect();
        let cues = if !words.is_empty() && joined_words == *text {
            Some(
                words
                    .iter()
                    .enumerate()
                    .map(|(word_index, word)| WordCue {
                        start_ms: word.0,
                        end_ms: word.1.unwrap_or_else(|| {
                            words.get(word_index + 1).map_or(line_end, |next| next.0)
                        }),
                        text: word.2.clone(),
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        if cues.as_ref().is_some_and(|cues| {
            cues.iter()
                .any(|cue| cue.end_ms < cue.start_ms || cue.end_ms > line_end)
        }) {
            return unavailable("Lyricsfile word timing is inconsistent.");
        }
        lines.push(LyricLine {
            start_ms: *start_ms,
            end_ms: line_end,
            text: text.clone(),
            words: cues,
        });
    }
    if lines.is_empty() {
        return if plain.trim().is_empty() {
            unavailable("Lyricsfile contains no lyrics.")
        } else {
            parse_plain(&plain)
        };
    }
    let word_line_count = lines.iter().filter(|line| line.words.is_some()).count();
    let quality = match word_line_count {
        0 => LyricsQuality::Line,
        count if count == lines.len() => LyricsQuality::Word,
        _ => LyricsQuality::Mixed,
    };
    LyricsResult {
        lines,
        quality,
        message: None,
    }
}

pub fn estimate_lyrics(result: &LyricsResult, duration_ms: Option<u64>) -> LyricsResult {
    if result.quality != LyricsQuality::Plain || duration_ms.is_none_or(|duration| duration == 0) {
        return result.clone();
    }

    let visible: Vec<_> = result
        .lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .collect();
    if visible.is_empty() {
        return result.clone();
    }
    let weights: Vec<f64> = visible
        .iter()
        .map(|line| (line.text.trim().chars().count() as f64).sqrt().max(1.0))
        .collect();
    let total_weight: f64 = weights.iter().sum();
    let duration = duration_ms.unwrap_or_default() as f64;
    let mut cumulative = 0.0;
    let lines = visible
        .iter()
        .zip(weights)
        .map(|(line, weight)| {
            let start_ms = (cumulative / total_weight * duration).round() as u64;
            cumulative += weight;
            let end_ms = (cumulative / total_weight * duration).round() as u64;
            LyricLine {
                start_ms,
                end_ms,
                text: line.text.clone(),
                words: None,
            }
        })
        .collect();

    LyricsResult {
        lines,
        quality: LyricsQuality::Estimated,
        message: Some("Estimated timing only; not synchronized to the audio.".into()),
    }
}

fn unavailable(message: &str) -> LyricsResult {
    LyricsResult {
        lines: Vec::new(),
        quality: LyricsQuality::Unavailable,
        message: Some(message.into()),
    }
}

fn parse_offset(source: &str) -> i128 {
    source
        .lines()
        .find_map(|line| {
            let line = line.trim_start();
            let rest = line.strip_prefix('[')?;
            let (tag, _) = rest.split_once(']')?;
            let (name, value) = tag.split_once(':')?;
            if !name.eq_ignore_ascii_case("offset") {
                return None;
            }
            let value = value.trim();
            let digits = value
                .strip_prefix('+')
                .or_else(|| value.strip_prefix('-'))
                .unwrap_or(value);
            if !(1..=8).contains(&digits.len()) || !digits.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            value.parse::<i128>().ok()
        })
        .unwrap_or(0)
}

fn apply_offset(timestamp_ms: u64, offset_ms: i128) -> u64 {
    (timestamp_ms as i128 + offset_ms).clamp(0, u64::MAX as i128) as u64
}

fn parse_ttml_time(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.contains(':') {
        let fields: Vec<_> = value.split(':').collect();
        if fields.len() != 3 {
            return None;
        }
        let hours = fields[0].parse::<u64>().ok()?;
        let minutes = fields[1].parse::<u64>().ok()?;
        let seconds = fields[2].parse::<f64>().ok()?;
        if minutes >= 60 || !(0.0..60.0).contains(&seconds) || !seconds.is_finite() {
            return None;
        }
        let total_ms =
            (hours as f64 * 3_600_000.0) + (minutes as f64 * 60_000.0) + (seconds * 1_000.0);
        return total_ms.is_finite().then_some(total_ms.round() as u64);
    }

    let (number, scale) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000.0)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000.0)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000.0)
    } else {
        return None;
    };
    let number = number.trim().parse::<f64>().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let total_ms = number * scale;
    total_ms.is_finite().then_some(total_ms.round() as u64)
}

fn ttml_parent_offset(node: roxmltree::Node<'_, '_>) -> Option<(u64, bool)> {
    let mut ancestors: Vec<_> = node
        .ancestors()
        .filter(|ancestor| ancestor.is_element() && *ancestor != node)
        .collect();
    ancestors.reverse();

    let mut offset_ms = 0u64;
    let mut has_timing = false;
    for ancestor in ancestors {
        if let Some(begin) = ancestor.attribute("begin") {
            offset_ms = offset_ms.saturating_add(parse_ttml_time(begin)?);
            has_timing = true;
        }
    }
    Some((offset_ms, has_timing))
}

fn ttml_word_cues(
    paragraph: roxmltree::Node<'_, '_>,
    line_start_ms: u64,
    line_text: &str,
) -> Vec<RawWord> {
    let mut candidates = Vec::new();
    for span in paragraph.descendants().filter(|node| {
        node.is_element() && node.tag_name().name() == "span" && node.attribute("begin").is_some()
    }) {
        let mut ancestor = span.parent();
        let mut nested_timing = false;
        while let Some(node) = ancestor {
            if node == paragraph {
                break;
            }
            if node.is_element()
                && node.tag_name().name() == "span"
                && node.attribute("begin").is_some()
            {
                nested_timing = true;
                break;
            }
            ancestor = node.parent();
        }
        if nested_timing {
            continue;
        }

        let mut relative_start_ms = 0u64;
        let mut ancestor = Some(span);
        while let Some(node) = ancestor {
            if node == paragraph {
                break;
            }
            if node.is_element() && node.tag_name().name() == "span" {
                if let Some(begin) = node.attribute("begin") {
                    let Some(begin_ms) = parse_ttml_time(begin) else {
                        return Vec::new();
                    };
                    relative_start_ms = relative_start_ms.saturating_add(begin_ms);
                }
            }
            ancestor = node.parent();
        }

        let text = collect_xml_text(span).trim().to_owned();
        if text.is_empty() || text.split_whitespace().count() != 1 {
            return Vec::new();
        }
        candidates.push(RawWord {
            start_ms: line_start_ms.saturating_add(relative_start_ms),
            text,
        });
    }

    let mut words: Vec<RawWord> = Vec::with_capacity(candidates.len());
    let mut cursor = 0;
    for mut candidate in candidates {
        let Some(relative) = line_text
            .get(cursor..)
            .and_then(|remaining| remaining.find(&candidate.text))
        else {
            return Vec::new();
        };
        let start = cursor + relative;
        let prefix = &line_text[cursor..start];
        if !prefix.trim().is_empty() {
            return Vec::new();
        }
        let matched_end = start + candidate.text.len();
        if let Some(previous) = words.last_mut() {
            previous.text.push_str(prefix);
        } else {
            candidate.text = format!("{prefix}{}", candidate.text);
        }
        cursor = matched_end;
        words.push(candidate);
    }

    let Some(suffix) = line_text.get(cursor..) else {
        return Vec::new();
    };
    if !suffix.trim().is_empty() {
        return Vec::new();
    }
    if let Some(last) = words.last_mut() {
        last.text.push_str(suffix);
    }
    let reconstructed: String = words.iter().map(|word| word.text.as_str()).collect();
    if reconstructed == line_text {
        words
    } else {
        Vec::new()
    }
}

fn collect_xml_text(node: roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|descendant| descendant.is_text())
        .filter_map(|descendant| descendant.text())
        .collect()
}

fn parse_stamp(text: &str, opening: u8) -> Option<(u64, usize)> {
    let bytes = text.as_bytes();
    if bytes.first().copied()? != opening {
        return None;
    }
    let closing = if opening == b'[' { b']' } else { b'>' };
    let mut index = 1;
    let minutes_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let minute_digits = index - minutes_start;
    if !(1..=6).contains(&minute_digits) || bytes.get(index) != Some(&b':') {
        return None;
    }
    let minutes = text[minutes_start..index].parse::<u64>().ok()?;
    index += 1;

    let seconds_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let second_digits = index - seconds_start;
    if !(1..=2).contains(&second_digits) {
        return None;
    }
    let seconds = text[seconds_start..index].parse::<u64>().ok()?;
    if seconds > 59 {
        return None;
    }

    let mut milliseconds = 0;
    if matches!(bytes.get(index), Some(b'.' | b':')) {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        let digits = index - fraction_start;
        if !(1..=3).contains(&digits) {
            return None;
        }
        let fraction = text[fraction_start..index].parse::<u64>().ok()?;
        milliseconds = fraction * 10u64.pow((3 - digits) as u32);
    }
    if bytes.get(index) != Some(&closing) {
        return None;
    }
    let total_ms = minutes
        .saturating_mul(60_000)
        .saturating_add(seconds.saturating_mul(1_000))
        .saturating_add(milliseconds);
    Some((total_ms, index + 1))
}

fn parse_inline_anchors(content: &str) -> Vec<InlineAnchor> {
    let mut anchors = Vec::new();
    let mut cursor = 0;
    while cursor < content.len() {
        let Some(relative) = content[cursor..].find('<') else {
            break;
        };
        let start = cursor + relative;
        if let Some((time_ms, consumed)) = parse_stamp(&content[start..], b'<') {
            anchors.push(InlineAnchor {
                start,
                end: start + consumed,
                time_ms,
            });
            cursor = start + consumed;
        } else {
            cursor = start + 1;
        }
    }
    anchors
}

fn remove_anchors(content: &str, anchors: &[InlineAnchor]) -> String {
    let mut output = String::with_capacity(content.len());
    let mut cursor = 0;
    for anchor in anchors {
        output.push_str(&content[cursor..anchor.start]);
        cursor = anchor.end;
    }
    output.push_str(&content[cursor..]);
    output
}

fn parse_word_cues(
    content: &str,
    anchors: &[InlineAnchor],
    line_start_ms: u64,
    delta_ms: i128,
) -> (Vec<RawWord>, Option<u64>) {
    if anchors.is_empty() {
        return (Vec::new(), None);
    }

    let mut words = Vec::new();
    let prefix = &content[..anchors[0].start];
    if !prefix.trim().is_empty() {
        words.push(RawWord {
            start_ms: line_start_ms,
            text: prefix.trim_start().to_owned(),
        });
    }

    let mut previous_start = line_start_ms;
    let mut end_ms = None;
    for (index, anchor) in anchors.iter().enumerate() {
        let word_start_ms = (anchor.time_ms as i128 + delta_ms).clamp(0, u64::MAX as i128) as u64;
        if word_start_ms < previous_start {
            return (Vec::new(), None);
        }
        previous_start = word_start_ms;
        let next_start = anchors.get(index + 1).map(|next| next.start);
        let segment_end = next_start.unwrap_or(content.len());
        let segment = &content[anchor.end..segment_end];
        if !segment.trim().is_empty() || next_start.is_some() {
            words.push(RawWord {
                start_ms: word_start_ms,
                text: segment.to_owned(),
            });
        } else if !words.is_empty() {
            end_ms = Some(word_start_ms);
        }
    }

    if let Some(first) = words.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = words.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    (words, end_ms)
}

fn timed_result(mut cues: Vec<RawCue>, duration_ms: Option<u64>) -> LyricsResult {
    cues.sort_by_key(|cue| cue.start_ms);
    let mut inferred_ends = vec![0; cues.len()];
    let mut next_distinct_start = None;
    for index in (0..cues.len()).rev() {
        let cue = &cues[index];
        if cues
            .get(index + 1)
            .is_some_and(|next| next.start_ms > cue.start_ms)
        {
            next_distinct_start = cues.get(index + 1).map(|next| next.start_ms);
        }
        inferred_ends[index] = next_distinct_start
            .or_else(|| duration_ms.filter(|duration| *duration > cue.start_ms))
            .unwrap_or_else(|| cue.start_ms.saturating_add(4_000));
    }

    let mut lines = Vec::with_capacity(cues.len());
    for (cue, inferred_end) in cues.iter().zip(inferred_ends) {
        let word_boundary = cue
            .words
            .iter()
            .map(|word| word.start_ms.saturating_add(1))
            .max()
            .unwrap_or(cue.start_ms);
        let end_ms = cue
            .end_ms
            .unwrap_or_else(|| inferred_end.max(word_boundary));
        if cue.text.trim().is_empty() {
            continue;
        }

        let word_text: String = cue.words.iter().map(|word| word.text.as_str()).collect();
        let words = if cue.words.is_empty() || word_text != cue.text {
            None
        } else {
            let words: Vec<_> = cue
                .words
                .iter()
                .enumerate()
                .map(|(word_index, word)| WordCue {
                    start_ms: word.start_ms,
                    end_ms: cue
                        .words
                        .get(word_index + 1)
                        .map_or(end_ms, |next| next.start_ms),
                    text: word.text.clone(),
                })
                .collect();
            let valid = words.iter().enumerate().all(|(word_index, word)| {
                word.start_ms >= cue.start_ms
                    && word.end_ms >= word.start_ms
                    && word.end_ms <= end_ms
                    && (word_index == 0 || word.start_ms >= words[word_index - 1].start_ms)
            });
            valid.then_some(words)
        };
        lines.push(LyricLine {
            start_ms: cue.start_ms,
            end_ms,
            text: cue.text.clone(),
            words,
        });
    }

    if lines.is_empty() {
        return unavailable("No timed lyrics in the file.");
    }
    let word_line_count = lines.iter().filter(|line| line.words.is_some()).count();
    let quality = match word_line_count {
        0 => LyricsQuality::Line,
        count if count == lines.len() => LyricsQuality::Word,
        _ => LyricsQuality::Mixed,
    };
    LyricsResult {
        lines,
        quality,
        message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        estimate_lyrics, parse_lrc, parse_lyricsfile, parse_plain, parse_ttml, LyricsQuality,
    };

    #[test]
    fn parses_lyricsfile_word_timing_and_preserves_mixed_quality() {
        let parsed = parse_lyricsfile(
            r#"version: "1.0"
metadata:
  title: "Test song"
  artist: "Test artist"
  duration_ms: 5000
lines:
  - text: "Hello world"
    start_ms: 1000
    end_ms: 2400
    words:
      - text: "Hello "
        start_ms: 1000
        end_ms: 1600
      - text: "world"
        start_ms: 1600
        end_ms: 2400
  - text: "Again"
    start_ms: 3000
plain: |
  Hello world
  Again
"#,
            None,
        );
        assert_eq!(parsed.quality, LyricsQuality::Mixed);
        assert_eq!(
            parsed.lines[0]
                .words
                .as_ref()
                .unwrap()
                .iter()
                .map(|word| (word.start_ms, word.end_ms, word.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(1000, 1600, "Hello "), (1600, 2400, "world")]
        );
        assert_eq!(parsed.lines[1].end_ms, 5000);
    }

    #[test]
    fn parses_lyricsfile_plain_and_instrumental_variants() {
        let plain = parse_lyricsfile(
            "version: '1.0'\nmetadata:\n  title: Song\n  artist: Artist\nplain: |\n  First line\n  Second line\n",
            None,
        );
        assert_eq!(plain.quality, LyricsQuality::Plain);
        assert_eq!(plain.lines[1].text, "Second line");

        let instrumental = parse_lyricsfile(
            "version: '1.0'\nmetadata:\n  title: Song\n  artist: Artist\n  instrumental: true\n",
            None,
        );
        assert_eq!(instrumental.quality, LyricsQuality::Instrumental);
    }

    #[test]
    fn rejects_lyricsfile_unsupported_or_unsafe_yaml() {
        for source in [
            "version: '2.0'\nmetadata: {title: Song, artist: Artist}\nplain: lyrics\n",
            "version: '1.0'\nmetadata: {title: Song, title: Other, artist: Artist}\nplain: lyrics\n",
            "version: '1.0'\nmetadata: &meta {title: Song, artist: Artist}\nplain: lyrics\n",
            "version: '1.0'\nmetadata: {title: Song, artist: Artist}\nplain: lyrics\n---\nother: document\n",
            "version: '1.0'\nmetadata: {title: Song, artist: Artist, offset_ms: 50}\nplain: lyrics\n",
        ] {
            assert_eq!(
                parse_lyricsfile(source, None).quality,
                LyricsQuality::Unavailable
            );
        }
    }

    #[test]
    fn sorts_repeated_lrc_stamps_applies_offset_and_keeps_blank_boundaries() {
        let parsed = parse_lrc(
            "\u{feff}[ar:Example]\r\n[00:03.00][00:01.00]Again\r\n[00:02.25]\r\n[offset:+50]",
            Some(5_000),
        );
        assert_eq!(parsed.quality, LyricsQuality::Line);
        assert_eq!(
            parsed
                .lines
                .iter()
                .map(|line| (line.start_ms, line.end_ms, line.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(1_050, 2_300, "Again"), (3_050, 5_000, "Again")]
        );
    }

    #[test]
    fn equal_start_lines_share_the_next_distinct_boundary() {
        let parsed = parse_lrc("[00:01]One\n[00:01]Two\n[00:06]Next", None);
        assert_eq!(
            parsed
                .lines
                .iter()
                .map(|line| (line.start_ms, line.end_ms))
                .collect::<Vec<_>>(),
            vec![(1_000, 6_000), (1_000, 6_000), (6_000, 10_000)]
        );
    }

    #[test]
    fn expands_repeated_enhanced_cues_and_trailing_word_end_stamps() {
        let parsed = parse_lrc(
            "[00:04.00][00:08.00]<00:04.00>Glow <00:04.45>now <00:05.63>",
            None,
        );
        assert_eq!(parsed.quality, LyricsQuality::Word);
        assert_eq!(parsed.lines.len(), 2);
        for (line, offset) in parsed.lines.iter().zip([0, 4_000]) {
            assert_eq!(line.start_ms, 4_000 + offset);
            assert_eq!(line.end_ms, 5_630 + offset);
            let words = line.words.as_ref().expect("enhanced words");
            assert_eq!(
                words
                    .iter()
                    .map(|word| (word.start_ms, word.end_ms, word.text.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    (4_000 + offset, 4_450 + offset, "Glow "),
                    (4_450 + offset, 5_630 + offset, "now")
                ]
            );
        }
    }

    #[test]
    fn invalid_word_order_falls_back_to_line_timing() {
        let parsed = parse_lrc("[00:02]<00:03>Moon <00:01>rise", None);
        assert_eq!(parsed.quality, LyricsQuality::Line);
        assert_eq!(parsed.lines[0].text, "Moon rise");
        assert!(parsed.lines[0].words.is_none());
    }

    #[test]
    fn parses_ttml_line_and_word_timing_with_mixed_quality() {
        let parsed = parse_ttml(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body begin="500ms"><div begin="0.5s">
                <p begin="00:00:01.000" end="00:00:03.000"><span begin="0s">Light</span> <span begin="1.2s">moves</span></p>
                <p begin="3s" dur="1s">Slowly</p>
            </div></body></tt>"#,
            None,
        );
        assert_eq!(parsed.quality, LyricsQuality::Mixed);
        assert_eq!(
            parsed
                .lines
                .iter()
                .map(|line| (line.start_ms, line.end_ms, line.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(2_000, 4_000, "Light moves"), (4_000, 5_000, "Slowly")]
        );
        assert_eq!(
            parsed.lines[0]
                .words
                .as_ref()
                .unwrap()
                .iter()
                .map(|word| (word.start_ms, word.end_ms, word.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(2_000, 3_200, "Light "), (3_200, 4_000, "moves")]
        );
        assert!(parsed.lines[1].words.is_none());
    }

    #[test]
    fn preserves_untimed_ttml_as_plain_text_and_rejects_invalid_timing() {
        let plain = parse_ttml(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p>Hello &amp; goodbye</p></div></body></tt>"#,
            None,
        );
        assert_eq!(plain.quality, LyricsQuality::Plain);
        assert_eq!(plain.lines[0].text, "Hello & goodbye");

        let body_only = parse_ttml(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><head><metadata><p>Not lyrics</p></metadata></head><body><p begin="1s">Song</p></body></tt>"#,
            None,
        );
        assert_eq!(body_only.lines.len(), 1);
        assert_eq!(body_only.lines[0].text, "Song");

        let unsupported_word_time = parse_ttml(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><p begin="1s" end="3s"><span begin="1f">Light</span> <span begin="2f">moves</span></p></body></tt>"#,
            None,
        );
        assert_eq!(unsupported_word_time.quality, LyricsQuality::Line);
        assert!(unsupported_word_time.lines[0].words.is_none());

        let invalid = parse_ttml(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><p begin="2f">unsupported</p></body></tt>"#,
            None,
        );
        assert_eq!(invalid.quality, LyricsQuality::Unavailable);

        let sequential = parse_ttml(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div timeContainer="seq">
                <p begin="0s">First</p><p begin="2s">Next</p>
            </div></body></tt>"#,
            None,
        );
        assert_eq!(sequential.quality, LyricsQuality::Plain);
        assert_eq!(sequential.lines.len(), 2);
    }

    #[test]
    fn preserves_plain_lyrics_and_estimates_only_when_explicitly_requested() {
        let plain = parse_plain("First\r\n\r\nSecond");
        assert_eq!(plain.quality, LyricsQuality::Plain);
        assert_eq!(
            plain
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["First", "", "Second"]
        );
        assert_eq!(estimate_lyrics(&plain, None), plain);
        let estimated = estimate_lyrics(&plain, Some(7_000));
        assert_eq!(estimated.quality, LyricsQuality::Estimated);
        assert_eq!(estimated.lines.first().unwrap().start_ms, 0);
        assert_eq!(estimated.lines.last().unwrap().end_ms, 7_000);
        assert!(estimated
            .message
            .as_deref()
            .unwrap()
            .contains("not synchronized"));
        assert_eq!(plain.quality, LyricsQuality::Plain);
    }

    #[test]
    fn rejects_empty_or_oversized_lyrics() {
        assert_eq!(parse_plain(" \n ").quality, LyricsQuality::Unavailable);
        assert_eq!(
            parse_lrc(&"x".repeat(256_001), None).quality,
            LyricsQuality::Unavailable
        );
        assert_eq!(
            parse_plain(&"x".repeat(256_001)).quality,
            LyricsQuality::Unavailable
        );
        assert_eq!(
            parse_lrc("[00:60]invalid timestamp", None).quality,
            LyricsQuality::Unavailable
        );
    }
}
