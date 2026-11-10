//! Excel 工作表命名规则。
//!
//! 这是原 SmartSQL（.NET/EPPlus 4.0.5）多 Sheet 导出的致命缺陷所在：
//! 它把数据库表名**原样**交给 `Worksheets.Add()`，而 Excel 的要求是
//!
//! * 长度 ≤ 31 个字符，超长会被静默截断；
//! * 不能包含 `: \ / ? * [ ]`；
//! * 不能为空，不能以单引号开头/结尾；
//! * 名称唯一，且**比较时不区分大小写**。
//!
//! EPPlus 截断 / 替换之后再去重，一旦两张表截断后同名（或仅大小写不同），
//! `Add()` 直接抛 `InvalidOperationException`，导出中止，文件也不会落盘 ——
//! 用户看到的就是「点了导出，什么都没有」。
//!
//! 本模块提供 [`SheetNameAllocator`]：对每个候选名做清洗、截断，并在冲突时
//! 追加 `_2` / `_3` 后缀，保证**永不抛异常、永不丢表**。

use std::collections::HashSet;

/// Excel 工作表名最大字符数。
pub const MAX_SHEET_NAME_LEN: usize = 31;

/// 原版固定写入的两个工作表名，业务表名撞上时要自动避让。
pub const RESERVED_SHEET_NAMES: [&str; 2] = ["目录", "修订日志"];

/// 分配唯一且合法的工作表名。
///
/// ```
/// use dbs_core::SheetNameAllocator;
///
/// let mut alloc = SheetNameAllocator::with_reserved(&["目录"]);
/// assert_eq!(alloc.allocate("t_user"), "t_user");
/// // 仅大小写不同也算重名
/// assert_eq!(alloc.allocate("T_USER"), "T_USER_2");
/// ```
#[derive(Debug, Clone, Default)]
pub struct SheetNameAllocator {
    /// 已占用名称的小写形式（Excel 去重不区分大小写）。
    used: HashSet<String>,
}

impl SheetNameAllocator {
    /// 创建一个不带任何预留名的分配器。
    pub fn new() -> Self {
        Self {
            used: HashSet::new(),
        }
    }

    /// 创建一个预先占用若干名称的分配器（用于「目录」「修订日志」等固定表）。
    pub fn with_reserved(names: &[&str]) -> Self {
        let mut alloc = Self::new();
        for n in names {
            alloc.used.insert(n.to_lowercase());
        }
        alloc
    }

    /// 当前已占用的名称数量。
    pub fn len(&self) -> usize {
        self.used.len()
    }

    pub fn is_empty(&self) -> bool {
        self.used.is_empty()
    }

    /// 是否已被占用（大小写不敏感）。
    pub fn is_taken(&self, name: &str) -> bool {
        self.used.contains(&name.to_lowercase())
    }

    /// 为 `preferred` 分配一个唯一、合法的工作表名。
    ///
    /// 冲突时追加 `_2`、`_3`……，并保证结果始终不超过 [`MAX_SHEET_NAME_LEN`] 个字符。
    pub fn allocate(&mut self, preferred: &str) -> String {
        let base = normalize(preferred);

        if !self.is_taken(&base) {
            self.used.insert(base.to_lowercase());
            return base;
        }

        // 从 _2 开始找可用后缀
        let mut n: u32 = 2;
        loop {
            let suffix = format!("_{n}");
            let keep = MAX_SHEET_NAME_LEN.saturating_sub(suffix.chars().count());
            let prefix = truncate_chars(&base, keep);
            let candidate = format!("{prefix}{suffix}");

            // 极端情况下（keep 为 0 时前缀为空）仍要保证非空
            if !candidate.is_empty() && !self.is_taken(&candidate) {
                self.used.insert(candidate.to_lowercase());
                return candidate;
            }
            n += 1;
        }
    }
}

/// 清洗并截断为合法的工作表名（不处理唯一性）。
fn normalize(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            // Excel 明确禁止的字符
            ':' | '\\' | '/' | '?' | '*' | '[' | ']' => '_',
            // 控制字符一并替换，避免生成损坏文件
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    // Excel 不允许名称以单引号开头或结尾
    let trimmed = sanitized.trim().trim_matches('\'').trim();
    let truncated = truncate_smart(trimmed, MAX_SHEET_NAME_LEN);

    if truncated.is_empty() {
        "Sheet".to_string()
    } else {
        truncated
    }
}

/// 超长时保留头部与尾部、中间用 `…` 省略。
///
/// 直接硬截断会把单词切一半（`..._generation_re`），而且丢掉表名尾部的区分词
/// （`questionnaire_answer` 只剩 `que`），导致多张同前缀的表难以分辨。
/// 这里头部保留约 1/3、尾部保留约 2/3 —— 数据库表名通常是
/// `t_模块_业务对象_动作` 的结构，尾部的信息量更大。
fn truncate_smart(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }

    let chars: Vec<char> = s.chars().collect();
    let head_len = max / 3;
    let tail_len = max - head_len - 1; // 1 个字符留给省略号

    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[count - tail_len..].iter().collect();
    format!("{head}…{tail}")
}

/// 按 **字符数** 截断，避免把 UTF-8 多字节字符切断。
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_name_kept_as_is() {
        let mut a = SheetNameAllocator::new();
        assert_eq!(a.allocate("t_user"), "t_user");
    }

    #[test]
    fn long_name_truncated_to_31() {
        let mut a = SheetNameAllocator::new();
        let long = "this_is_a_very_long_table_name_over_31_chars_001";
        let got = a.allocate(long);
        assert_eq!(got.chars().count(), MAX_SHEET_NAME_LEN);
        // 头部 + 省略号 + 尾部，保留结尾的区分信息
        assert_eq!(got, "this_is_a_…me_over_31_chars_001");
    }

    #[test]
    fn invalid_chars_replaced() {
        let mut a = SheetNameAllocator::new();
        assert_eq!(a.allocate("tbl:illegal/x"), "tbl_illegal_x");
        assert_eq!(a.allocate("a[b]c?d*e\\f"), "a_b_c_d_e_f");
    }

    #[test]
    fn empty_and_blank_get_fallback() {
        let mut a = SheetNameAllocator::new();
        assert_eq!(a.allocate(""), "Sheet");
        assert_eq!(a.allocate("   "), "Sheet_2");
        assert_eq!(a.allocate("'''"), "Sheet_3");
    }

    #[test]
    fn surrounding_quotes_removed() {
        let mut a = SheetNameAllocator::new();
        assert_eq!(a.allocate("'quoted'"), "quoted");
    }

    /// 用户真实库里的第一处冲突：仅大小写不同。
    #[test]
    fn case_insensitive_collision_gets_suffix() {
        let mut a = SheetNameAllocator::with_reserved(&RESERVED_SHEET_NAMES);
        assert_eq!(
            a.allocate("T_EXHIBITION_EVENT_CONFIG"),
            "T_EXHIBITION_EVENT_CONFIG"
        );
        assert_eq!(
            a.allocate("t_exhibition_event_config"),
            "t_exhibition_event_config_2"
        );
    }

    /// 用户真实库里的两张长表名之前会截断撞名，改成首尾保留后可以区分。
    #[test]
    fn truncation_no_longer_collides_when_tails_differ() {
        let mut a = SheetNameAllocator::new();
        let first = a.allocate("t_landlord_presale_whitelist_file");
        let second = a.allocate("t_landlord_presale_whitelist_file_member");

        assert_ne!(first, second, "尾部不同就不应再撞名");
        assert_eq!(first, "t_landlord…esale_whitelist_file");
        assert_eq!(second, "t_landlord…hitelist_file_member");
        for name in [&first, &second] {
            assert_eq!(name.chars().count(), MAX_SHEET_NAME_LEN);
        }
    }

    /// 真正同名（含大小写差异）时仍走 `_2` 后缀。
    #[test]
    fn identical_names_still_get_suffix() {
        let mut a = SheetNameAllocator::new();
        let first = a.allocate("t_very_long_table_name_that_will_be_truncated_tail");
        let second = a.allocate("t_very_long_table_name_that_will_be_truncated_tail");
        assert_ne!(first, second);
        assert!(second.ends_with("_2"), "实际：{second}");
        assert!(second.chars().count() <= MAX_SHEET_NAME_LEN);
    }

    /// 一组 4 张长表同名（questionnaire 系列），必须全部拿到独立名字。
    #[test]
    fn four_way_truncation_collision_all_unique() {
        let names = [
            "t_store_themed_activities_questionnaire",
            "t_store_themed_activities_questionnaire_answer",
            "t_store_themed_activities_questionnaire_option",
            "t_store_themed_activities_questionnaire_question",
        ];
        let mut a = SheetNameAllocator::new();
        let mut got: Vec<String> = names.iter().map(|n| a.allocate(n)).collect();
        assert_eq!(got.len(), 4);
        let unique: HashSet<String> = got.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(unique.len(), 4, "四个名字必须互不相同，实际：{got:?}");
        for g in &got {
            assert!(g.chars().count() <= MAX_SHEET_NAME_LEN, "{g} 超长");
        }
        got.sort();
        got.dedup();
        assert_eq!(got.len(), 4);
    }

    /// 业务表名撞上固定工作表「目录」时必须避让。
    #[test]
    fn reserved_name_avoided() {
        let mut a = SheetNameAllocator::with_reserved(&RESERVED_SHEET_NAMES);
        assert_eq!(a.allocate("目录"), "目录_2");
        assert_eq!(a.allocate("修订日志"), "修订日志_2");
    }

    #[test]
    fn chinese_names_counted_by_chars() {
        let mut a = SheetNameAllocator::new();
        // 用超过 31 个字符的中文名，验证按「字符」而非「字节」截断
        let name = "这是一个非常非常长的中文表名用来测试字符数截断逻辑的边界情况补充更多文字";
        assert!(name.chars().count() > MAX_SHEET_NAME_LEN);
        let got = a.allocate(name);
        assert_eq!(got.chars().count(), MAX_SHEET_NAME_LEN);
        // 截断后仍是合法 UTF-8（不会切坏汉字）
        assert!(got.chars().all(|c| !c.is_control()));
    }

    #[test]
    fn suffix_never_exceeds_limit() {
        let mut a = SheetNameAllocator::new();
        let long = "a".repeat(40);
        for _ in 0..150 {
            let got = a.allocate(&long);
            assert!(got.chars().count() <= MAX_SHEET_NAME_LEN, "{got} 超长");
        }
        assert_eq!(a.len(), 150);
    }

    /// 模拟真实库 390 张表的完整分配：必须零异常、零重名。
    #[test]
    fn allocate_many_realistic_names_stays_unique() {
        let mut a = SheetNameAllocator::with_reserved(&RESERVED_SHEET_NAMES);
        let mut all = Vec::new();
        // 普通表
        for i in 0..300 {
            all.push(format!("t_table_{i:04}"));
        }
        // 长表名（会被截断）
        for i in 0..60 {
            all.push(format!("t_some_very_long_table_name_number_{i:03}_suffix"));
        }
        // 仅大小写不同
        all.push("T_DUP".into());
        all.push("t_dup".into());
        all.push("t_Dup".into());

        let assigned: Vec<String> = all.iter().map(|n| a.allocate(n)).collect();
        let unique: HashSet<String> = assigned.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(unique.len(), assigned.len(), "存在重名，导出会失败");
        for s in &assigned {
            assert!(!s.is_empty());
            assert!(s.chars().count() <= MAX_SHEET_NAME_LEN);
        }
    }

    #[test]
    fn same_name_twice_allocates_distinct() {
        let mut a = SheetNameAllocator::new();
        assert_eq!(a.allocate("dup"), "dup");
        assert_eq!(a.allocate("dup"), "dup_2");
        assert_eq!(a.allocate("dup"), "dup_3");
    }
}
