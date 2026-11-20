//! 名称分词、注释分句与显式限定引用的纯文本解析。
//!
//! 这里只做语言层面的切分，不认识表或键；表身份解析在目录索引中完成。

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// 按词边界切分：分隔符、camelCase、字母/数字边界，CJK 逐字保留。
pub(crate) fn tokenize(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut ascii_start: Option<usize> = None;
    for (idx, ch) in input.char_indices() {
        if ch.is_ascii_alphanumeric() {
            if ascii_start.is_none() {
                ascii_start = Some(idx);
            }
            continue;
        }
        if let Some(start) = ascii_start.take() {
            push_ascii_tokens(&input[start..idx], start, &mut out);
        }
        if !ch.is_ascii() && ch.is_alphanumeric() {
            out.push(Token {
                text: ch.to_lowercase().collect(),
                start: idx,
                end: idx + ch.len_utf8(),
            });
        }
    }
    if let Some(start) = ascii_start {
        push_ascii_tokens(&input[start..], start, &mut out);
    }
    out
}

fn push_ascii_tokens(run: &str, base: usize, out: &mut Vec<Token>) {
    let chars: Vec<(usize, char)> = run.char_indices().collect();
    let mut start = 0usize;
    for i in 1..chars.len() {
        let prev = chars[i - 1].1;
        let cur = chars[i].1;
        let next = chars.get(i + 1).map(|(_, c)| *c);
        let boundary = (prev.is_lowercase() && cur.is_uppercase())
            || (prev.is_uppercase()
                && cur.is_uppercase()
                && next.is_some_and(|c| c.is_lowercase()))
            || (prev.is_alphabetic() != cur.is_alphabetic());
        if boundary {
            let (offset, _) = chars[start];
            let (end, _) = chars[i];
            out.push(Token {
                text: run[offset..end].to_ascii_lowercase(),
                start: base + offset,
                end: base + end,
            });
            start = i;
        }
    }
    if start < chars.len() {
        let (offset, _) = chars[start];
        out.push(Token {
            text: run[offset..].to_ascii_lowercase(),
            start: base + offset,
            end: base + run.len(),
        });
    }
}

pub(crate) fn tokens_of(input: &str) -> Vec<String> {
    tokenize(input).into_iter().map(|t| t.text).collect()
}

pub(crate) fn singular(token: &str) -> String {
    if token.ends_with("ies") && token.len() > 4 {
        return format!("{}y", &token[..token.len() - 3]);
    }
    if (token.ends_with("ses")
        || token.ends_with("xes")
        || token.ends_with("ches")
        || token.ends_with("shes"))
        && token.len() > 4
    {
        return token[..token.len() - 2].to_string();
    }
    if token.ends_with('s')
        && token.len() > 3
        && !token.ends_with("ss")
        && !token.ends_with("us")
        && !token.ends_with("is")
    {
        return token[..token.len() - 1].to_string();
    }
    token.to_string()
}

pub(crate) fn token_eq(a: &str, b: &str) -> bool {
    a == b || ((a.ends_with('s') || b.ends_with('s')) && singular(a) == singular(b))
}

fn sequence_eq(tokens: &[Token], at: usize, needle: &[String]) -> bool {
    if at + needle.len() > tokens.len() {
        return false;
    }
    needle
        .iter()
        .enumerate()
        .all(|(i, word)| token_eq(&tokens[at + i].text, word))
}

pub(crate) fn find_sequence(tokens: &[Token], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > tokens.len() {
        return None;
    }
    (0..=tokens.len() - needle.len()).find(|&at| sequence_eq(tokens, at, needle))
}

fn fixed_phrases() -> &'static std::collections::BTreeMap<&'static str, Vec<String>> {
    static WORDS: std::sync::OnceLock<std::collections::BTreeMap<&'static str, Vec<String>>> =
        std::sync::OnceLock::new();
    WORDS.get_or_init(|| {
        [
            REFERENCE_PHRASES,
            NEGATION_PHRASES,
            HISTORY_PHRASES,
            CONDITIONAL_PHRASES,
            EXTERNAL_PHRASES,
            PURPOSE_PHRASES,
            GENERIC_PHRASES,
        ]
        .into_iter()
        .flatten()
        .map(|&phrase| (phrase, tokens_of(phrase)))
        .collect()
    })
}

pub(crate) fn contains_phrase(tokens: &[Token], phrase: &str) -> bool {
    if tokens.is_empty() {
        return false;
    }
    if let Some(needle) = fixed_phrases().get(phrase) {
        return find_sequence(tokens, needle).is_some();
    }
    let needle = tokens_of(phrase);
    find_sequence(tokens, &needle).is_some()
}

/// 去掉名称末尾的完整键用途短语，例如 `编号`、`id`、`业务键`。
/// 返回被去掉的 token 数，便于调用方判断名称是否只剩用途词。
pub(crate) fn trim_purpose_suffix(tokens: &mut Vec<String>) -> usize {
    let mut best = 0usize;
    for phrase in PURPOSE_PHRASES {
        let needle = &fixed_phrases()[phrase];
        if needle.len() > best
            && tokens.len() >= needle.len()
            && tokens[tokens.len() - needle.len()..]
                .iter()
                .zip(needle)
                .all(|(a, b)| a == b)
        {
            best = needle.len();
        }
    }
    if best > 0 {
        tokens.truncate(tokens.len() - best);
    }
    best
}

pub(crate) fn contains_any(tokens: &[Token], phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| contains_phrase(tokens, phrase))
}

/// 分句时忽略括号与引号内部的标点，避免拆开复合端点。
pub(crate) fn split_clauses(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (idx, ch) in text.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '`' => quote = Some(ch),
            '(' | '（' | '[' | '【' => depth += 1,
            ')' | '）' | ']' | '】' => depth = depth.saturating_sub(1),
            '，' | '。' | '；' | ';' | '!' | '?' | '！' | '？' | '\n' | '\r' => {
                if depth == 0 {
                    let part = text[start..idx].trim();
                    if !part.is_empty() {
                        out.push(part);
                    }
                    start = idx + ch.len_utf8();
                }
            }
            ',' if depth == 0 => {
                let part = text[start..idx].trim();
                if !part.is_empty() {
                    out.push(part);
                }
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    if quote.is_none() {
        let part = text[start..].trim();
        if !part.is_empty() {
            out.push(part);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub(crate) struct RawRef {
    pub quoted: bool,
    pub path: Vec<String>,
    pub columns: Vec<String>,
    pub start: usize,
    pub end: usize,
}

pub(crate) fn parse_explicit_refs(text: &str) -> Vec<RawRef> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let (start_byte, ch) = chars[i];
        if !(is_ident_char(ch) || ch == '"' || ch == '\'' || ch == '`' || ch == '[') {
            i += 1;
            continue;
        }
        let Some((path, mut next)) = parse_path(&chars, i) else {
            i += 1;
            continue;
        };
        let after_path = skip_space(&chars, next);
        let mut columns = Vec::new();
        let mut end = next;
        if after_path < chars.len() && chars[after_path].1 == '(' {
            if let Some((cols, after)) = parse_column_list(&chars, after_path) {
                columns = cols;
                next = after;
                end = after;
            }
        }
        let has_columns = !columns.is_empty();
        if path.len() >= 2 || has_columns {
            let end_byte = if end < chars.len() {
                chars[end].0
            } else {
                text.len()
            };
            out.push(RawRef {
                quoted: text[start_byte..end_byte].contains(['"', '\'', '[', '`']),
                path,
                columns,
                start: start_byte,
                end: end_byte,
            });
            i = next.max(i + 1);
        } else {
            i = next.max(i + 1);
        }
    }
    out
}

fn parse_path(chars: &[(usize, char)], start: usize) -> Option<(Vec<String>, usize)> {
    let mut segments = Vec::new();
    let mut i = start;
    loop {
        let (segment, next) = parse_segment(chars, i)?;
        if segment.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        segments.push(segment);
        i = skip_space(chars, next);
        if i < chars.len() && chars[i].1 == '.' {
            i += 1;
            i = skip_space(chars, i);
            continue;
        }
        break;
    }
    if segments.is_empty() {
        None
    } else {
        Some((segments, i))
    }
}

fn parse_segment(chars: &[(usize, char)], start: usize) -> Option<(String, usize)> {
    if start >= chars.len() {
        return None;
    }
    let opener = chars[start].1;
    if opener == '"' || opener == '\'' || opener == '`' || opener == '[' {
        let closer = match opener {
            '[' => ']',
            other => other,
        };
        let mut i = start + 1;
        let mut value = String::new();
        while i < chars.len() {
            if chars[i].1 == closer {
                return if value.is_empty() {
                    None
                } else {
                    Some((value, i + 1))
                };
            }
            value.push(chars[i].1);
            i += 1;
        }
        return None;
    }
    if !is_ident_char(opener) || opener.is_ascii_digit() {
        return None;
    }
    let mut i = start;
    let mut value = String::new();
    while i < chars.len() && is_ident_char(chars[i].1) {
        value.push(chars[i].1);
        i += 1;
    }
    if value.is_empty() {
        None
    } else {
        Some((value, i))
    }
}

fn parse_column_list(chars: &[(usize, char)], open: usize) -> Option<(Vec<String>, usize)> {
    let mut columns = Vec::new();
    let mut i = open + 1;
    loop {
        i = skip_space(chars, i);
        if i >= chars.len() {
            return None;
        }
        if chars[i].1 == ')' {
            if columns.is_empty() {
                return None;
            }
            return Some((columns, i + 1));
        }
        let (column, next) = parse_segment(chars, i)?;
        columns.push(column);
        i = skip_space(chars, next);
        if i >= chars.len() {
            return None;
        }
        match chars[i].1 {
            ',' => i += 1,
            ')' => return Some((columns, i + 1)),
            _ => return None,
        }
    }
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '#'
}

fn skip_space(chars: &[(usize, char)], mut i: usize) -> usize {
    while i < chars.len() && chars[i].1.is_whitespace() {
        i += 1;
    }
    i
}

pub(crate) fn spans_overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// 去掉端点标识符覆盖的 token；限定路径里的表名/列名不参与“历史、条件、否定”判断。
pub(crate) fn tokens_outside_spans(tokens: &[Token], spans: &[(usize, usize)]) -> Vec<Token> {
    tokens
        .iter()
        .filter(|token| {
            !spans
                .iter()
                .any(|&span| spans_overlap((token.start, token.end), span))
        })
        .cloned()
        .collect()
}

const REFERENCE_PHRASES: &[&str] = &[
    "引用",
    "关联",
    "指向",
    "对应",
    "外键",
    "隶属",
    "属于",
    "取自",
    "来源于",
    "references",
    "reference",
    "refers to",
    "refer to",
    "points to",
    "point to",
    "points at",
    "links to",
    "link to",
    "maps to",
    "map to",
    "relates to",
    "relate to",
    "belongs to",
    "belong to",
    "owned by",
    "foreign key",
    "fk",
];
const NEGATION_PHRASES: &[&str] = &[
    "不是",
    "并非",
    "不引用",
    "不关联",
    "不指向",
    "无引用",
    "无关联",
    "没有引用",
    "not",
    "not a",
    "is not",
    "isn't",
    "does not",
    "doesn't",
    "never",
    "no reference",
    "not reference",
    "not references",
];
const HISTORY_PHRASES: &[&str] = &[
    "历史",
    "曾经",
    "已废弃",
    "已弃用",
    "旧",
    "historical",
    "history",
    "legacy",
    "deprecated",
    "previously",
];
const CONDITIONAL_PHRASES: &[&str] = &[
    "如果",
    "若",
    "条件",
    "仅在",
    "取决于",
    "仅当",
    "每当",
    "when",
    "if",
    "only when",
    "conditional",
    "depending on",
];
const CONDITIONAL_COMPOUND_PREFIX: &[&str] = &[
    "前", "时", "天", "然", "中", "下", "地", "面", "作", "场", "即", "初", "晚",
];
const EXTERNAL_PHRASES: &[&str] = &[
    "外部",
    "第三方",
    "外部系统",
    "另一系统",
    "external",
    "third-party",
    "third party",
];
const PURPOSE_PHRASES: &[&str] = &[
    "主键",
    "标识",
    "编号",
    "编码",
    "代码",
    "业务号",
    "序号",
    "业务键",
    "id",
    "pk",
    "identity",
    "code",
    "no",
    "num",
    "number",
    "uuid",
    "guid",
    "key",
];
const GENERIC_PHRASES: &[&str] = &[
    "字段",
    "列",
    "属性",
    "当前",
    "用于",
    "用来",
    "表示",
    "存储",
    "保存",
    "内容",
    "值",
    "该表",
    "此表",
    "本表",
    "当前表",
    "记录",
    "类型",
    "类别",
    "情况",
    "场景",
    "时候",
    "条件",
    "当",
    "时",
    "field",
    "column",
    "value",
    "current",
    "record",
    "records",
    "stores",
    "store",
    "holds",
    "hold",
    "used",
    "for",
    "as",
    "to",
    "the",
    "a",
    "an",
    "of",
    "is",
    "are",
    "this",
    "that",
    "it",
];

pub(crate) fn has_reference_intent(tokens: &[Token]) -> bool {
    contains_any(tokens, REFERENCE_PHRASES)
}

pub(crate) fn is_negated(tokens: &[Token]) -> bool {
    contains_any(tokens, NEGATION_PHRASES)
}

pub(crate) fn is_historical(tokens: &[Token]) -> bool {
    contains_any(tokens, HISTORY_PHRASES)
}

pub(crate) fn is_conditional(tokens: &[Token]) -> bool {
    if contains_any(tokens, CONDITIONAL_PHRASES) {
        return true;
    }
    let needle = tokens_of("当");
    if needle.is_empty() {
        return false;
    }
    for at in 0..=tokens.len().saturating_sub(needle.len()) {
        if !sequence_eq(tokens, at, &needle) {
            continue;
        }
        let next = tokens.get(at + needle.len()).map(|t| t.text.as_str());
        // “当前”“当时”“当然”等是普通词，不是条件从句。
        if !next.is_some_and(|next| CONDITIONAL_COMPOUND_PREFIX.contains(&next)) {
            return true;
        }
    }
    false
}

pub(crate) fn is_external(tokens: &[Token]) -> bool {
    contains_any(tokens, EXTERNAL_PHRASES)
}

/// 从注释中提取角色短语：去掉端点、引用谓词、用途词和泛化描述，保留原文语序。
pub(crate) fn role_tokens(text: &str, explicit: &[(usize, usize)]) -> Vec<String> {
    let mut spans = explicit.to_vec();
    let tokens = tokenize(text);
    // 静态语言规则与库数据无关；编译一次，避免每个字段逐词分配。
    let vocabulary = fixed_phrases();
    for needle in vocabulary.values() {
        if needle.is_empty() {
            continue;
        }
        for at in 0..=tokens.len().saturating_sub(needle.len()) {
            if sequence_eq(&tokens, at, needle) {
                spans.push((tokens[at].start, tokens[at + needle.len() - 1].end));
            }
        }
    }
    let mut cleaned = String::with_capacity(text.len());
    for (idx, ch) in text.char_indices() {
        let blanked = spans.iter().any(|&(s, e)| idx >= s && idx < e);
        if blanked || !ch.is_alphanumeric() {
            cleaned.push(' ');
        } else {
            cleaned.push(ch);
        }
    }
    tokens_of(&cleaned)
        .into_iter()
        .filter(|token| !token.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

pub(crate) fn extract_domain(text: &str) -> Option<String> {
    for marker in ["业务域", "所属域", "领域", "domain"] {
        let Some(at) = find_marker(text, marker) else {
            continue;
        };
        let after_marker = at + marker.len();
        let rest = &text[after_marker..];
        let had_space = rest.chars().next().is_some_and(|c| c.is_whitespace());
        let rest_trimmed = rest.trim_start();
        let mut consumed = rest.len() - rest_trimmed.len();
        let Some(first) = rest_trimmed.chars().next() else {
            continue;
        };
        let delimiter_len =
            if matches!(first, ':' | '：' | '=' | '＝') || first == '为' || first == '是' {
                first.len_utf8()
            } else if had_space {
                0
            } else {
                continue;
            };
        consumed += delimiter_len;
        let value_start = after_marker + consumed;
        let value_end = text[value_start..]
            .char_indices()
            .find(|(_, c)| {
                matches!(
                    c,
                    '，' | ','
                        | '。'
                        | ';'
                        | '；'
                        | '!'
                        | '?'
                        | '！'
                        | '？'
                        | '\n'
                        | ')'
                        | '）'
                        | ']'
                        | '】'
                )
            })
            .map(|(i, _)| value_start + i)
            .unwrap_or(text.len());
        let value = text[value_start..value_end]
            .trim()
            .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '[' | '【' | '(' | '（' | ' '))
            .trim_end_matches([' ', ']', '】', ')', '）']);
        if !value.is_empty() {
            return Some(value.to_lowercase());
        }
    }
    None
}

fn find_marker(text: &str, marker: &str) -> Option<usize> {
    if marker.is_ascii() {
        let lower = text.to_ascii_lowercase();
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(marker) {
            let at = from + rel;
            let before_ok = at == 0
                || !text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric());
            let after = at + marker.len();
            let after_ok = after >= text.len()
                || !lower[after..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric());
            if before_ok && after_ok {
                return Some(at);
            }
            from = at + marker.len().max(1);
        }
        None
    } else {
        text.find(marker)
    }
}

/// 去掉表说明内的业务域声明，避免把域值当作实体别名。
pub(crate) fn strip_domain(text: &str) -> String {
    for marker in ["业务域", "所属域", "领域", "domain"] {
        if let Some(at) = find_marker(text, marker) {
            let end = extract_domain_end(text, at + marker.len());
            let padding = " ".repeat(text[at..end].chars().count());
            return format!("{}{}{}", &text[..at], padding, &text[end..]);
        }
    }
    text.to_string()
}

fn extract_domain_end(text: &str, from: usize) -> usize {
    let rest = &text[from..];
    let mut consumed = 0usize;
    if let Some((_, first)) = rest.char_indices().next() {
        if first.is_whitespace() || matches!(first, ':' | '：' | '=' | '＝' | '为' | '是') {
            consumed = first.len_utf8();
        }
    }
    let value_start = from + consumed;
    text[value_start..]
        .char_indices()
        .find(|(_, c)| {
            matches!(
                c,
                '，' | ','
                    | '。'
                    | ';'
                    | '；'
                    | '!'
                    | '?'
                    | '！'
                    | '？'
                    | '\n'
                    | ')'
                    | '）'
                    | ']'
                    | '】'
            )
        })
        .map(|(i, _)| value_start + i)
        .unwrap_or(text.len())
}
