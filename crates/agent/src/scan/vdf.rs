//! Valve's KeyValues text format (`libraryfolders.vdf`, `appmanifest_*.acf`):
//! quoted keys, quoted values or braced blocks, `//` comments, backslash
//! escapes.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Block(Vec<(String, Value)>),
}

impl Value {
    /// The first child named [`key`], ignoring case as Steam does.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Block(items) => items.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Value::Text(_) => None,
        }
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Text(t) => Some(t),
            Value::Block(_) => None,
        }
    }

    pub fn children(&self) -> &[(String, Value)] {
        match self {
            Value::Block(items) => items,
            Value::Text(_) => &[],
        }
    }
}

enum Token {
    Str(String),
    Open,
    Close,
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => out.push(Token::Open),
            '}' => out.push(Token::Close),
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => {}
                        },
                        '"' => break,
                        other => s.push(other),
                    }
                }
                out.push(Token::Str(s));
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            c => {
                // An unquoted token, as some tools write.
                let mut s = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || n == '{' || n == '}' || n == '"' {
                        break;
                    }
                    s.push(n);
                    chars.next();
                }
                out.push(Token::Str(s));
            }
        }
    }
    out
}

/// The whole file as one block.
pub fn parse(text: &str) -> Value {
    let toks = tokens(text);
    let mut pos = 0;
    Value::Block(block(&toks, &mut pos))
}

fn block(toks: &[Token], pos: &mut usize) -> Vec<(String, Value)> {
    let mut items = Vec::new();
    while *pos < toks.len() {
        match &toks[*pos] {
            Token::Close => {
                *pos += 1;
                break;
            }
            Token::Open => {
                *pos += 1;
            }
            Token::Str(key) => {
                *pos += 1;
                match toks.get(*pos) {
                    Some(Token::Str(v)) => {
                        *pos += 1;
                        items.push((key.clone(), Value::Text(v.clone())));
                    }
                    Some(Token::Open) => {
                        *pos += 1;
                        items.push((key.clone(), Value::Block(block(toks, pos))));
                    }
                    _ => break,
                }
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_library_folders_and_manifests() {
        let text = r#"
"libraryfolders"
{
    "0"
    {
        "path"		"C:\\Program Files (x86)\\Steam"
        "apps" { "440" "123" }
    }
    // a comment
    "1" { "path" "D:\\SteamLibrary" }
}"#;
        let v = parse(text);
        let lf = v.get("LibraryFolders").unwrap();
        assert_eq!(lf.get("0").unwrap().text("path").unwrap(), r"C:\Program Files (x86)\Steam");
        assert_eq!(lf.get("1").unwrap().text("PATH").unwrap(), r"D:\SteamLibrary");
        let acf = parse(r#""AppState" { "appid" "440" "name" "Team \"Fortress\" 2" "StateFlags" "4" }"#);
        assert_eq!(acf.get("AppState").unwrap().text("name").unwrap(), "Team \"Fortress\" 2");
    }
}
