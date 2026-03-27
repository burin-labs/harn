use std::collections::BTreeMap;

use harn_runtime::Value;

/// Parse a JSON string into a Harn Value.
pub fn json_parse(input: &str) -> Result<Value, String> {
    let mut parser = JsonParser::new(input);
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos < parser.chars.len() {
        return Err(format!(
            "Unexpected trailing content at position {}",
            parser.pos
        ));
    }
    Ok(value)
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_whitespace();
        match self.peek() {
            Some('"') => self.parse_string(),
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('t') => self.parse_literal("true", Value::Bool(true)),
            Some('f') => self.parse_literal("false", Value::Bool(false)),
            Some('n') => self.parse_literal("null", Value::Nil),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!(
                "Unexpected character '{}' at position {}",
                c, self.pos
            )),
            None => Err("Unexpected end of JSON".to_string()),
        }
    }

    fn parse_string(&mut self) -> Result<Value, String> {
        Ok(Value::String(self.parse_string_raw()?))
    }

    fn parse_string_raw(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut s = String::new();
        loop {
            match self.next() {
                Some('"') => return Ok(s),
                Some('\\') => match self.next() {
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('/') => s.push('/'),
                    Some('n') => s.push('\n'),
                    Some('r') => s.push('\r'),
                    Some('t') => s.push('\t'),
                    Some('b') => s.push('\u{0008}'),
                    Some('f') => s.push('\u{000C}'),
                    Some('u') => {
                        let hex = self.take_n(4)?;
                        let cp = u32::from_str_radix(&hex, 16)
                            .map_err(|_| format!("Invalid unicode escape: \\u{hex}"))?;
                        // Handle UTF-16 surrogate pairs
                        let ch = if (0xD800..=0xDBFF).contains(&cp) {
                            // High surrogate: expect \uXXXX low surrogate
                            if self.peek() == Some('\\') && self.peek_at(1) == Some('u') {
                                self.next(); // consume '\'
                                self.next(); // consume 'u'
                                let hex2 = self.take_n(4)?;
                                let cp2 = u32::from_str_radix(&hex2, 16)
                                    .map_err(|_| format!("Invalid unicode escape: \\u{hex2}"))?;
                                if (0xDC00..=0xDFFF).contains(&cp2) {
                                    let combined = 0x10000 + ((cp - 0xD800) << 10) + (cp2 - 0xDC00);
                                    char::from_u32(combined).ok_or_else(|| {
                                        format!("Invalid surrogate pair: \\u{hex}\\u{hex2}")
                                    })?
                                } else {
                                    return Err(format!(
                                        "Expected low surrogate after \\u{hex}, got \\u{hex2}"
                                    ));
                                }
                            } else {
                                return Err(format!(
                                    "Expected low surrogate after high surrogate \\u{hex}"
                                ));
                            }
                        } else {
                            char::from_u32(cp)
                                .ok_or_else(|| format!("Invalid unicode codepoint: {cp}"))?
                        };
                        s.push(ch);
                    }
                    Some(c) => return Err(format!("Invalid escape sequence: \\{c}")),
                    None => return Err("Unterminated string".to_string()),
                },
                Some(c) => s.push(c),
                None => return Err("Unterminated string".to_string()),
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.peek() == Some('.') {
            is_float = true;
            self.pos += 1;
            while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.pos += 1;
            }
            while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                self.pos += 1;
            }
        }
        let num_str: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            let n: f64 = num_str
                .parse()
                .map_err(|_| format!("Invalid number: {num_str}"))?;
            Ok(Value::Float(n))
        } else {
            let n: i64 = num_str
                .parse()
                .map_err(|_| format!("Invalid number: {num_str}"))?;
            Ok(Value::Int(n))
        }
    }

    fn parse_object(&mut self) -> Result<Value, String> {
        self.expect('{')?;
        self.skip_whitespace();
        let mut map = BTreeMap::new();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Dict(map));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string_raw()?;
            self.skip_whitespace();
            self.expect(':')?;
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                }
                Some('}') => {
                    self.pos += 1;
                    return Ok(Value::Dict(map));
                }
                _ => return Err(format!("Expected ',' or '}}' at position {}", self.pos)),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Value, String> {
        self.expect('[')?;
        self.skip_whitespace();
        let mut items = Vec::new();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::List(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_whitespace();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                }
                Some(']') => {
                    self.pos += 1;
                    return Ok(Value::List(items));
                }
                _ => return Err(format!("Expected ',' or ']' at position {}", self.pos)),
            }
        }
    }

    fn parse_literal(&mut self, expected: &str, value: Value) -> Result<Value, String> {
        for ch in expected.chars() {
            match self.next() {
                Some(c) if c == ch => {}
                _ => return Err(format!("Expected '{expected}' at position {}", self.pos)),
            }
        }
        Ok(value)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn expect(&mut self, expected: char) -> Result<(), String> {
        match self.next() {
            Some(c) if c == expected => Ok(()),
            Some(c) => Err(format!(
                "Expected '{expected}', got '{c}' at position {}",
                self.pos - 1
            )),
            None => Err(format!("Expected '{expected}', got end of input")),
        }
    }

    fn take_n(&mut self, n: usize) -> Result<String, String> {
        if self.pos + n > self.chars.len() {
            return Err("Unexpected end of input".to_string());
        }
        let s: String = self.chars[self.pos..self.pos + n].iter().collect();
        self.pos += n;
        Ok(s)
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .map(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r')
            .unwrap_or(false)
        {
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_string() {
        assert_eq!(
            json_parse(r#""hello""#).unwrap(),
            Value::String("hello".into())
        );
    }

    #[test]
    fn test_parse_number() {
        assert_eq!(json_parse("42").unwrap(), Value::Int(42));
        assert_eq!(json_parse("-5").unwrap(), Value::Int(-5));
        assert_eq!(json_parse("3.14").unwrap(), Value::Float(3.14));
    }

    #[test]
    fn test_parse_bool_null() {
        assert_eq!(json_parse("true").unwrap(), Value::Bool(true));
        assert_eq!(json_parse("false").unwrap(), Value::Bool(false));
        assert_eq!(json_parse("null").unwrap(), Value::Nil);
    }

    #[test]
    fn test_parse_array() {
        let val = json_parse("[1, 2, 3]").unwrap();
        assert_eq!(
            val,
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn test_parse_object() {
        let val = json_parse(r#"{"a": 1, "b": "two"}"#).unwrap();
        if let Value::Dict(map) = val {
            assert_eq!(map.get("a"), Some(&Value::Int(1)));
            assert_eq!(map.get("b"), Some(&Value::String("two".into())));
        } else {
            panic!("Expected dict");
        }
    }

    #[test]
    fn test_parse_nested() {
        let val = json_parse(r#"{"items": [1, {"x": true}], "count": 2}"#).unwrap();
        if let Value::Dict(map) = val {
            assert_eq!(map.get("count"), Some(&Value::Int(2)));
        } else {
            panic!("Expected dict");
        }
    }

    #[test]
    fn test_parse_escapes() {
        let val = json_parse(r#""hello\nworld""#).unwrap();
        assert_eq!(val, Value::String("hello\nworld".into()));
    }

    #[test]
    fn test_parse_empty() {
        assert_eq!(json_parse("{}").unwrap(), Value::Dict(BTreeMap::new()));
        assert_eq!(json_parse("[]").unwrap(), Value::List(Vec::new()));
    }
}
