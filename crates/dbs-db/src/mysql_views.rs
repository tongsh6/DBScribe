//! 只识别 MySQL `information_schema.VIEWS.VIEW_DEFINITION` 中可完整还原的
//! 两表内连接等值视图。
//!
//! MySQL 会把视图定义规范化成带库名、反引号、别名和多层括号的 SELECT。
//! 这里按完整语法解析，不截取 `ON` 子串、不做字符串替换；遇到函数、常量、
//! 额外谓词、外库、外连接、OR 或任何无法定位的标识一律弃权。

use dbs_core::er::{Endpoint, TableId};
use dbs_core::TableInfo;

/// 完整解析出的普通/空安全等值列对。
pub(crate) struct ParsedView {
    pub left: Endpoint,
    pub right: Endpoint,
    /// 视图至少包含一个 `<=>`；整条视图只作为负向证据。
    pub null_safe: bool,
}

enum Token {
    Name(String, bool),
    Symbol(&'static str),
}

fn lex(sql: &str) -> Option<Vec<Token>> {
    let mut chars = sql.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if matches!(c, '(' | ')' | ',' | '.' | '*' | ';' | '=') {
            tokens.push(Token::Symbol(match c {
                '(' => "(",
                ')' => ")",
                ',' => ",",
                '.' => ".",
                '*' => "*",
                ';' => ";",
                _ => "=",
            }));
            continue;
        }
        if c == '<' {
            if chars.next_if_eq(&'=').is_some() && chars.next_if_eq(&'>').is_some() {
                tokens.push(Token::Symbol("<=>"));
                continue;
            }
            return None;
        }
        if c == '`' {
            let mut name = String::new();
            loop {
                match chars.next()? {
                    '`' if chars.next_if_eq(&'`').is_some() => name.push('`'),
                    '`' => break,
                    other => name.push(other),
                }
            }
            if name.is_empty() {
                return None;
            }
            tokens.push(Token::Name(name, true));
            continue;
        }
        if !(c.is_ascii_alphabetic() || c == '_' || c == '$') {
            // 字符串、数字、注释或其他运算符都不在可完整还原的范围内。
            return None;
        }
        let mut name = String::from(c);
        while let Some(n) = chars.next_if(|n| n.is_ascii_alphanumeric() || *n == '_' || *n == '$') {
            name.push(n);
        }
        tokens.push(Token::Name(name, false));
    }
    Some(tokens)
}

struct Input<'a>(&'a [Token]);

impl<'a> Input<'a> {
    fn kw(&mut self, word: &str) -> bool {
        match self.0.split_first() {
            Some((Token::Name(name, false), rest)) if name.eq_ignore_ascii_case(word) => {
                self.0 = rest;
                true
            }
            _ => false,
        }
    }

    fn sym(&mut self, symbol: &str) -> bool {
        match self.0.split_first() {
            Some((Token::Symbol(found), rest)) if *found == symbol => {
                self.0 = rest;
                true
            }
            _ => false,
        }
    }

    fn name(&mut self) -> Option<&'a str> {
        const RESERVED: &[&str] = &[
            "SELECT",
            "FROM",
            "AS",
            "INNER",
            "JOIN",
            "ON",
            "AND",
            "OR",
            "WHERE",
            "LEFT",
            "RIGHT",
            "FULL",
            "CROSS",
            "NATURAL",
            "USING",
            "GROUP",
            "ORDER",
            "HAVING",
            "LIMIT",
            "OFFSET",
            "DISTINCT",
            "UNION",
            "STRAIGHT_JOIN",
            "OUTER",
            "NULL",
            "IS",
            "COLLATE",
            "INTERVAL",
            "CASE",
            "WHEN",
            "THEN",
            "ELSE",
            "END",
            "EXISTS",
            "IN",
            "LIKE",
            "BETWEEN",
            "ASC",
            "DESC",
        ];
        let (Token::Name(name, quoted), rest) = self.0.split_first()? else {
            return None;
        };
        if !quoted && RESERVED.iter().any(|word| name.eq_ignore_ascii_case(word)) {
            return None;
        }
        self.0 = rest;
        Some(name)
    }

    /// 读取最多 `max` 段的限定名；超过上限视为无法完整还原。
    fn qualified(&mut self, max: usize) -> Option<Vec<&'a str>> {
        let mut parts = vec![self.name()?];
        while self.sym(".") {
            if parts.len() == max {
                return None;
            }
            parts.push(self.name()?);
        }
        Some(parts)
    }

    /// 只接受普通列/限定列/`*` 投影，并以 `FROM` 结束。
    fn projection(&mut self) -> Option<()> {
        loop {
            if !self.sym("*") {
                self.name()?;
                if self.sym(".") && !self.sym("*") {
                    self.name()?;
                    if self.sym(".") {
                        self.name()?;
                    }
                }
                if self.kw("AS") {
                    self.name()?;
                }
            }
            if !self.sym(",") {
                return self.kw("FROM").then_some(());
            }
        }
    }

    fn binding<'t>(&mut self, schema: &str, tables: &'t [TableInfo]) -> Option<Binding<'t>> {
        let parts = self.qualified(2)?;
        let table = find_table(schema, &parts, tables)?;
        let alias = if self.kw("AS") {
            Some(self.name()?.to_string())
        } else {
            self.name().map(str::to_string)
        };
        Some(Binding { table, alias })
    }

    fn condition<'t>(&mut self, schema: &str, bindings: &[Binding<'t>]) -> Option<Vec<Pair>> {
        let mut pairs = Vec::new();
        let mut depth = 0usize;
        loop {
            while self.sym("(") {
                depth += 1;
            }
            let left = resolve_column(schema, &self.qualified(3)?, bindings)?;
            let null_safe = self.sym("<=>");
            if !null_safe && !self.sym("=") {
                return None;
            }
            let right = resolve_column(schema, &self.qualified(3)?, bindings)?;
            pairs.push(Pair {
                left,
                right,
                null_safe,
            });
            while depth > 0 && self.sym(")") {
                depth -= 1;
            }
            if !self.kw("AND") {
                break;
            }
        }
        (depth == 0).then_some(pairs)
    }
}

#[derive(Clone)]
struct Binding<'t> {
    table: &'t TableInfo,
    alias: Option<String>,
}

struct Pair {
    left: (usize, String),
    right: (usize, String),
    null_safe: bool,
}

fn find_table<'t>(schema: &str, parts: &[&str], tables: &'t [TableInfo]) -> Option<&'t TableInfo> {
    let (table_schema, name) = match parts {
        [name] => (schema, *name),
        [table_schema, name] => (*table_schema, *name),
        _ => return None,
    };
    // 在区分大小写的 MySQL 实例中，库名只差大小写也可以是两个不同的库。
    if table_schema != schema {
        return None;
    }
    let mut matches = tables
        .iter()
        .filter(|table| table.schema == table_schema && table.name.eq_ignore_ascii_case(name));
    let table = matches.next()?;
    matches.next().is_none().then_some(table)
}

fn column_name<'t>(table: &'t TableInfo, column: &str) -> Option<&'t str> {
    let mut matches = table
        .columns
        .iter()
        .filter(|meta| meta.name.eq_ignore_ascii_case(column));
    let found = matches.next()?;
    matches.next().is_none().then_some(found.name.as_str())
}

fn resolve_column(
    schema: &str,
    parts: &[&str],
    bindings: &[Binding<'_>],
) -> Option<(usize, String)> {
    let column = *parts.last()?;
    let indexes: Vec<usize> = match parts {
        [_] => bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| column_name(binding.table, column).is_some())
            .map(|(index, _)| index)
            .collect(),
        [qualifier, _] => {
            let mut hits: Vec<usize> = bindings
                .iter()
                .enumerate()
                .filter(|(_, binding)| {
                    binding
                        .alias
                        .as_deref()
                        .is_some_and(|alias| alias.eq_ignore_ascii_case(qualifier))
                })
                .map(|(index, _)| index)
                .collect();
            hits.extend(
                bindings
                    .iter()
                    .enumerate()
                    .filter(|(_, binding)| {
                        binding.alias.is_none()
                            && binding.table.name.eq_ignore_ascii_case(qualifier)
                    })
                    .map(|(index, _)| index),
            );
            hits
        }
        [table_schema, qualifier, _] => {
            if *table_schema != schema {
                return None;
            }
            bindings
                .iter()
                .enumerate()
                .filter(|(_, binding)| {
                    binding.alias.is_none() && binding.table.name.eq_ignore_ascii_case(qualifier)
                })
                .map(|(index, _)| index)
                .collect()
        }
        _ => return None,
    };
    if indexes.len() != 1 {
        return None;
    }
    let index = indexes[0];
    let actual = column_name(bindings[index].table, column)?;
    Some((index, actual.to_string()))
}

/// 解析一条规范化视图定义；任何不确定都返回 `None`。
pub(crate) fn comparison(
    schema: &str,
    definition: &str,
    tables: &[TableInfo],
) -> Option<ParsedView> {
    let tokens = lex(definition)?;
    let mut sql = Input(&tokens);
    if !sql.kw("SELECT") {
        return None;
    }
    sql.projection()?;

    // MySQL 5.7 常把整个 join 包在括号里；允许有限层数，防止异常输入。
    let mut wrappers = 0;
    while sql.sym("(") {
        wrappers += 1;
        if wrappers > 8 {
            return None;
        }
    }
    let first = sql.binding(schema, tables)?;
    sql.kw("INNER");
    if !sql.kw("JOIN") {
        return None;
    }
    let second = sql.binding(schema, tables)?;
    if TableId::from(first.table) == TableId::from(second.table) {
        return None;
    }
    if !sql.kw("ON") {
        return None;
    }
    let bindings = [first.clone(), second.clone()];
    let pairs = sql.condition(schema, &bindings)?;
    for _ in 0..wrappers {
        if !sql.sym(")") {
            return None;
        }
    }
    sql.sym(";");
    if !sql.0.is_empty() {
        return None;
    }

    let mut left_columns = Vec::new();
    let mut right_columns = Vec::new();
    let mut null_safe = false;
    for pair in pairs {
        let (left, right) = match (pair.left.0, pair.right.0) {
            (0, 1) => (pair.left.1, pair.right.1),
            (1, 0) => (pair.right.1, pair.left.1),
            _ => return None,
        };
        if left_columns.contains(&left) || right_columns.contains(&right) {
            return None;
        }
        left_columns.push(left);
        right_columns.push(right);
        null_safe |= pair.null_safe;
    }
    if left_columns.is_empty() {
        return None;
    }

    Some(ParsedView {
        left: Endpoint {
            table: TableId::from(first.table),
            columns: left_columns,
        },
        right: Endpoint {
            table: TableId::from(second.table),
            columns: right_columns,
        },
        null_safe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::ColumnInfo;

    fn table(name: &str, columns: &[&str]) -> TableInfo {
        TableInfo {
            schema: "app".into(),
            name: name.into(),
            columns: columns
                .iter()
                .map(|column| ColumnInfo {
                    name: (*column).into(),
                    data_type: "x".into(),
                    length: String::new(),
                    is_primary_key: false,
                    is_auto_increment: false,
                    nullable: false,
                    default_value: String::new(),
                    comment: String::new(),
                    order: 1,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_backticks_schema_aliases_parentheses_and_and_pairs() {
        let tables = vec![
            table("orders", &["id", "tenant_id", "customer_id"]),
            table("customers", &["id", "tenant_id"]),
        ];
        let parsed = comparison(
            "app",
            "select `o`.`tenant_id` AS `tenant_id`,`c`.`id` AS `customer_id` \
             from (`app`.`orders` `o` join `app`.`customers` `c` \
             on(((`o`.`tenant_id` = `c`.`tenant_id`) and (`o`.`customer_id` = `c`.`id`))))",
            &tables,
        )
        .expect("真实规范化形态应可解析");
        assert_eq!(parsed.left.columns, ["tenant_id", "customer_id"]);
        assert_eq!(parsed.right.columns, ["tenant_id", "id"]);
        assert!(!parsed.null_safe);
    }

    #[test]
    fn null_safe_marker_applies_to_the_whole_view() {
        let tables = vec![table("a", &["x", "y"]), table("b", &["x", "y"])];
        let parsed = comparison(
            "app",
            "select `a`.`x` AS `x` from `app`.`a` join `app`.`b` \
             on(((`a`.`x` = `b`.`x`) and (`a`.`y` <=> `b`.`y`)))",
            &tables,
        )
        .expect("混合比较应可解析");
        assert!(parsed.null_safe);
    }

    #[test]
    fn abstains_on_outer_join_or_functions_or_cross_schema() {
        let tables = vec![table("a", &["x"]), table("b", &["x"])];
        let unsupported = [
            "select `a`.`x` AS `x` from `app`.`a` left join `app`.`b` on((`a`.`x` = `b`.`x`))",
            "select `a`.`x` AS `x` from `app`.`a` join `app`.`b` on((`a`.`x` = `b`.`x`) or (`a`.`x` = `b`.`x`))",
            "select `a`.`x` AS `x` from `app`.`a` join `app`.`b` on((lower(`a`.`x`) = `b`.`x`))",
            "select `a`.`x` AS `x` from other.`a` join `app`.`b` on((`a`.`x` = `b`.`x`))",
            "select a.x from `APP`.a join app.b on a.x = b.x",
            "select `a`.`x` AS `x` from `app`.`a` join `app`.`b` on((`a`.`x` = `b`.`x`)) where 1",
        ];
        for definition in unsupported {
            assert!(
                comparison("app", definition, &tables).is_none(),
                "不应接受：{definition}"
            );
        }
    }
}
