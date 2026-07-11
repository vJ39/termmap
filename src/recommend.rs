// ツーリングのおすすめスポットをローカルの Claude CLI から取得してパースするモジュール。
// std のみで完結する（外部 crate 不使用・crate:: 参照無し・単体で rustc コンパイル可能）。

use std::collections::HashMap;
use std::process::Command;

/// Claude CLI から返るおすすめスポット1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rec {
    pub name: String,
    pub area: String,
}

/// 寛容な JSON パーサ用の内部値表現。
/// name/area 以外のフィールドが混じっても崩れないように、最低限の JSON 値種別を持つ。
#[derive(Debug, Clone)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(HashMap<String, JsonValue>),
}

/// 文字配列上を前から読み進める最小限の JSON パーサ。
struct Parser<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(chars: &'a [char], pos: usize) -> Self {
        Parser { chars, pos }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn consume_literal(&mut self, lit: &str) -> bool {
        let lit_chars: Vec<char> = lit.chars().collect();
        let end = self.pos + lit_chars.len();
        if end > self.chars.len() {
            return false;
        }
        if self.chars[self.pos..end] == lit_chars[..] {
            self.pos = end;
            true
        } else {
            false
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_ws();
        match self.peek() {
            Some('"') => self.parse_string().map(JsonValue::String),
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('t') | Some('f') => self.parse_bool(),
            Some('n') => self.parse_null(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("予期しない文字です: {}", c)),
            None => Err("入力が途中で終端しました".to_string()),
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.advance() != Some('"') {
            return Err("文字列の開始が不正です".to_string());
        }
        let mut out = String::new();
        loop {
            match self.advance() {
                Some('"') => return Ok(out),
                Some('\\') => match self.advance() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('b') => out.push('\u{0008}'),
                    Some('f') => out.push('\u{000C}'),
                    Some('u') => {
                        let mut hex = String::new();
                        for _ in 0..4 {
                            match self.advance() {
                                Some(h) => hex.push(h),
                                None => return Err("\\uエスケープが不正です".to_string()),
                            }
                        }
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|_| "\\uエスケープが不正です".to_string())?;
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                    Some(other) => out.push(other),
                    None => return Err("文字列が終端していません".to_string()),
                },
                Some(c) => out.push(c),
                None => return Err("文字列が終端していません".to_string()),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.advance(); // '{'
        let mut map = HashMap::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.advance();
            return Ok(JsonValue::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err("オブジェクトのキーが文字列ではありません".to_string());
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.advance() != Some(':') {
                return Err("オブジェクトに ':' がありません".to_string());
            }
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.advance() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err("オブジェクトの終端が不正です".to_string()),
            }
        }
        Ok(JsonValue::Object(map))
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.advance(); // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.advance();
            return Ok(JsonValue::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.advance() {
                Some(',') => continue,
                Some(']') => break,
                _ => return Err("配列の終端が不正です".to_string()),
            }
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_bool(&mut self) -> Result<JsonValue, String> {
        if self.consume_literal("true") {
            Ok(JsonValue::Bool(true))
        } else if self.consume_literal("false") {
            Ok(JsonValue::Bool(false))
        } else {
            Err("真偽値が不正です".to_string())
        }
    }

    fn parse_null(&mut self) -> Result<JsonValue, String> {
        if self.consume_literal("null") {
            Ok(JsonValue::Null)
        } else {
            Err("null が不正です".to_string())
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.advance();
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-' {
                self.advance();
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err("数値が不正です".to_string());
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        Ok(JsonValue::Number(s))
    }
}

/// Claude CLI の出力を寛容にパースし、おすすめスポットの一覧を返す。
///
/// 前後に説明文が付いていたり ```json コードフェンスで囲まれていても、
/// 文字列中の最初の `[` から始まる JSON 配列を探して解釈する。
/// 配列が見つからない・空・要素がオブジェクトでない等の場合は Err を返す。
pub fn parse_rec_json(s: &str) -> Result<Vec<Rec>, String> {
    let chars: Vec<char> = s.chars().collect();
    let start = chars
        .iter()
        .position(|&c| c == '[')
        .ok_or_else(|| "JSON配列が見つかりません".to_string())?;

    let mut parser = Parser::new(&chars, start);
    let value = parser.parse_array()?;
    let items = match value {
        JsonValue::Array(items) => items,
        _ => unreachable!("parse_array は必ず JsonValue::Array を返す"),
    };

    if items.is_empty() {
        return Err("JSON配列が空です".to_string());
    }

    let mut recs = Vec::with_capacity(items.len());
    for item in items {
        match item {
            JsonValue::Object(map) => {
                let name = match map.get("name") {
                    Some(JsonValue::String(n)) => n.clone(),
                    _ => String::new(),
                };
                let area = match map.get("area") {
                    Some(JsonValue::String(a)) => a.clone(),
                    _ => String::new(),
                };
                recs.push(Rec { name, area });
            }
            _ => return Err("配列要素がオブジェクトではありません".to_string()),
        }
    }

    Ok(recs)
}

/// 指定した Claude CLI コマンドが起動可能かを確認する。
/// `<command> --version` の実行が成功すれば true。PATH に無い・実行不可なら false。
pub fn claude_available(command: &str) -> bool {
    Command::new(command).arg("--version").output().is_ok()
}

/// Claude CLI にツーリングのおすすめスポットを問い合わせ、パースして返す。
pub fn recommend(command: &str, model: &str, direction: &str) -> Result<Vec<Rec>, String> {
    let prompt = format!(
        "あなたはツーリングのおすすめスポットを挙げる。次の方向性に合う実在の場所/道を最大8件、JSON配列のみで返す。各要素は {{name, area}}。前置き・説明・コードフェンス無し。方向性: {}",
        direction
    );

    let output = Command::new(command)
        .arg("-p")
        .arg("--no-session-persistence")
        .arg("--model")
        .arg(model)
        .arg(&prompt)
        .output()
        .map_err(|e| format!("Claude CLI の起動に失敗しました: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Claude CLI がエラー終了しました (status: {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_rec_json(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_array() {
        let input = r#"[{"name":"ヤビツ峠","area":"神奈川"},{"name":"椿ライン","area":"静岡"}]"#;
        let got = parse_rec_json(input).expect("clean array should parse");
        assert_eq!(
            got,
            vec![
                Rec { name: "ヤビツ峠".to_string(), area: "神奈川".to_string() },
                Rec { name: "椿ライン".to_string(), area: "静岡".to_string() },
            ]
        );
    }

    #[test]
    fn parses_array_wrapped_in_prose_and_code_fence() {
        let input = r#"はい、おすすめは以下の通りです。

```json
[
  {"name": "しらびそ峠", "area": "長野"},
  {"name": "野麦峠", "area": "長野"}
]
```

参考にしてください。"#;
        let got = parse_rec_json(input).expect("fenced array should parse");
        assert_eq!(
            got,
            vec![
                Rec { name: "しらびそ峠".to_string(), area: "長野".to_string() },
                Rec { name: "野麦峠".to_string(), area: "長野".to_string() },
            ]
        );
    }

    #[test]
    fn missing_area_becomes_empty_string() {
        let input = r#"[{"name":"某峠"}]"#;
        let got = parse_rec_json(input).expect("object without area should parse");
        assert_eq!(
            got,
            vec![Rec { name: "某峠".to_string(), area: String::new() }]
        );
    }

    #[test]
    fn garbage_input_is_err() {
        assert!(parse_rec_json("これはJSONではありません、山でも行きますか").is_err());
        assert!(parse_rec_json("[not valid json at all]").is_err());
        assert!(parse_rec_json("[]").is_err());
    }
}
