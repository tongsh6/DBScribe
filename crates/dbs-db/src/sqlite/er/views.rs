//! 仅识别两个基础表之间、没有行过滤的直接等值内连接。其他视图不给推断器提供端点。
use dbs_core::er::{Endpoint, ErCatalog, TableId, ViewComparison};
use dbs_core::TableInfo;

#[derive(PartialEq)]
enum Token {
    Name(String, bool),
    Symbol(char),
}

fn lex(sql: &str) -> Option<Vec<Token>> {
    let mut chars = sql.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if ".,=*;".contains(c) {
            tokens.push(Token::Symbol(c));
            continue;
        }
        if matches!(c, '"' | '[' | '`') {
            let end = if c == '[' { ']' } else { c };
            let mut name = String::new();
            loop {
                let next = chars.next()?;
                if next == end {
                    if end != ']' && chars.next_if_eq(&end).is_some() {
                        name.push(end);
                        continue;
                    }
                    break;
                }
                name.push(next);
            }
            tokens.push(Token::Name(name, true));
            continue;
        }
        if !(c.is_alphanumeric() || c == '_') {
            return None;
        }
        let mut name = String::from(c);
        while let Some(n) = chars.next_if(|n| n.is_alphanumeric() || *n == '_' || *n == '$') {
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
            Some((Token::Name(s, false), rest)) if s.eq_ignore_ascii_case(word) => {
                self.0 = rest;
                true
            }
            _ => false,
        }
    }
    fn sym(&mut self, c: char) -> bool {
        if self.0.first() == Some(&Token::Symbol(c)) {
            self.0 = &self.0[1..];
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Option<&'a str> {
        let (Token::Name(name, quoted), rest) = self.0.split_first()? else {
            return None;
        };
        const RESERVED: &[&str] = &[
            "SELECT", "FROM", "AS", "INNER", "JOIN", "ON", "AND", "WHERE", "OR", "LIMIT", "GROUP",
            "ORDER", "DISTINCT", "UNION", "LEFT", "RIGHT", "FULL", "CROSS", "COLLATE", "HAVING",
            "OFFSET", "NATURAL",
        ];
        if !quoted && RESERVED.iter().any(|w| name.eq_ignore_ascii_case(w)) {
            return None;
        }
        self.0 = rest;
        Some(name)
    }
    fn projection(&mut self) -> Option<()> {
        loop {
            if !self.sym('*') {
                self.name()?;
                if self.sym('.') && !self.sym('*') {
                    self.name()?;
                }
                if self.kw("AS") {
                    self.name()?;
                }
            }
            if !self.sym(',') {
                return self.kw("FROM").then_some(());
            }
        }
    }
    fn binding<'t>(&mut self, tables: &'t [TableInfo]) -> Option<(&'a str, &'t TableInfo)> {
        let mut name = self.name()?;
        if self.sym('.') {
            if !name.eq_ignore_ascii_case("main") {
                return None;
            }
            name = self.name()?;
        }
        let table = tables.iter().find(|t| t.name.eq_ignore_ascii_case(name))?;
        let alias = if self.kw("AS") {
            self.name()?
        } else {
            self.name().unwrap_or(name)
        };
        Some((alias, table))
    }
    fn column(&mut self) -> Option<(&'a str, &'a str)> {
        let table = self.name()?;
        self.sym('.').then_some(())?;
        Some((table, self.name()?))
    }
}

pub(super) fn comparison(catalog: &ErCatalog, name: &str, ddl: &str) -> Option<ViewComparison> {
    let tokens = lex(ddl)?;
    let mut sql = Input(&tokens);
    if !(sql.kw("CREATE") && sql.kw("VIEW")) {
        return None;
    }
    sql.name()?;
    if !(sql.kw("AS") && sql.kw("SELECT")) {
        return None;
    }
    sql.projection()?;
    let (a, left) = sql.binding(&catalog.tables)?;
    sql.kw("INNER");
    if !sql.kw("JOIN") {
        return None;
    }
    let (b, right) = sql.binding(&catalog.tables)?;
    if a.eq_ignore_ascii_case(b) || !sql.kw("ON") {
        return None;
    }
    let mut left_cols = Vec::new();
    let mut right_cols = Vec::new();
    loop {
        let (l, lc) = sql.column()?;
        if !sql.sym('=') {
            return None;
        }
        let (r, rc) = sql.column()?;
        let (lc, rc) = if l.eq_ignore_ascii_case(a) && r.eq_ignore_ascii_case(b) {
            (lc, rc)
        } else if l.eq_ignore_ascii_case(b) && r.eq_ignore_ascii_case(a) {
            (rc, lc)
        } else {
            return None;
        };
        let lc = &left
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(lc))?
            .name;
        let rc = &right
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(rc))?
            .name;
        if left_cols.contains(lc) || right_cols.contains(rc) {
            return None;
        }
        left_cols.push(lc.clone());
        right_cols.push(rc.clone());
        if !sql.kw("AND") {
            break;
        }
    }
    sql.sym(';');
    if !sql.0.is_empty() {
        return None;
    }
    let mut left = Endpoint {
        table: TableId::from(left),
        columns: left_cols,
    };
    let mut right = Endpoint {
        table: TableId::from(right),
        columns: right_cols,
    };
    // SQL 的 AND 项可交换，仍须保留真实列对并按目标键位序重排。
    for key in &catalog.keys {
        let names = if key.table == left.table {
            &left.columns
        } else if key.table == right.table {
            &right.columns
        } else {
            continue;
        };
        if names.len() != key.columns.len() {
            continue;
        }
        let order: Option<Vec<_>> = key
            .columns
            .iter()
            .map(|c| names.iter().position(|n| n == c))
            .collect();
        if let Some(order) = order {
            left.columns = order.iter().map(|&i| left.columns[i].clone()).collect();
            right.columns = order.iter().map(|&i| right.columns[i].clone()).collect();
            break;
        }
    }
    Some(ViewComparison {
        view: TableId {
            schema: "main".into(),
            name: name.into(),
        },
        left,
        right,
    })
}
