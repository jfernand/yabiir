//! From text to [`Select`]: a tokenizer and a recursive-descent parser, one
//! function per rule of the grammar.
//!
//! ```text
//! select    := SELECT [DISTINCT] fields [WHERE condition]
//! fields    := '*' | key | value
//! condition := key '=' string
//!            | key BETWEEN string AND string
//! string    := "'" { any character, with '' for a quote } "'"
//! ```
//!
//! Keywords and field names are case-insensitive.

use std::fmt;

use super::ast::{Condition, Fields, Select};

/// Why a query didn't parse, and where: `at` is a byte offset into the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub at: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.at, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A keyword or a field name, upper-cased.
    Word(String),
    Star,
    Equals,
    String(Vec<u8>),
    End,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Word(w) => write!(f, "{w}"),
            Token::Star => f.write_str("'*'"),
            Token::Equals => f.write_str("'='"),
            Token::String(_) => f.write_str("a string"),
            Token::End => f.write_str("the end of the query"),
        }
    }
}

/// The text cut into tokens, each with the offset it starts at.
fn tokenize(text: &str) -> Result<Vec<(usize, Token)>, ParseError> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b if b.is_ascii_whitespace() => i += 1,
            b'*' => {
                tokens.push((start, Token::Star));
                i += 1;
            }
            b'=' => {
                tokens.push((start, Token::Equals));
                i += 1;
            }
            b'\'' => {
                let mut value = Vec::new();
                i += 1;
                loop {
                    match bytes.get(i) {
                        None => {
                            return Err(ParseError {
                                at: start,
                                message: "string is never closed".into(),
                            });
                        }
                        Some(b'\'') if bytes.get(i + 1) == Some(&b'\'') => {
                            value.push(b'\'');
                            i += 2;
                        }
                        Some(b'\'') => {
                            i += 1;
                            break;
                        }
                        Some(&b) => {
                            value.push(b);
                            i += 1;
                        }
                    }
                }
                tokens.push((start, Token::String(value)));
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                tokens.push((start, Token::Word(text[start..i].to_ascii_uppercase())));
            }
            _ => {
                let c = text[start..]
                    .chars()
                    .next()
                    .unwrap_or('?');
                return Err(ParseError {
                    at: start,
                    message: format!("unexpected {c:?}"),
                });
            }
        }
    }
    tokens.push((text.len(), Token::End));
    Ok(tokens)
}

struct Parser {
    tokens: Vec<(usize, Token)>,
    next: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.next].1
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.next]
            .1
            .clone();
        if token != Token::End {
            self.next += 1;
        }
        token
    }

    fn error(&self, expected: &str) -> ParseError {
        let (at, found) = &self.tokens[self.next];
        ParseError {
            at: *at,
            message: format!("expected {expected}, found {found}"),
        }
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Token::Word(w) if w == word)
    }

    fn expect_word(&mut self, word: &str) -> Result<(), ParseError> {
        if self.is_word(word) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(word))
        }
    }

    fn expect_string(&mut self) -> Result<Vec<u8>, ParseError> {
        match self.peek() {
            Token::String(_) => match self.advance() {
                Token::String(s) => Ok(s),
                _ => unreachable!(),
            },
            _ => Err(self.error("a string in single quotes")),
        }
    }

    /// `select := SELECT [DISTINCT] fields [WHERE condition]`
    fn select(&mut self) -> Result<Select, ParseError> {
        self.expect_word("SELECT")?;
        let distinct = self.is_word("DISTINCT");
        if distinct {
            self.advance();
        }
        let fields = self.fields()?;
        let condition = if self.is_word("WHERE") {
            self.advance();
            Some(self.condition()?)
        } else {
            None
        };
        if *self.peek() != Token::End {
            return Err(self.error(if condition.is_none() {
                "WHERE or the end"
            } else {
                "the end"
            }));
        }
        Ok(Select {
            distinct,
            fields,
            condition,
        })
    }

    /// `fields := '*' | key | value`
    fn fields(&mut self) -> Result<Fields, ParseError> {
        let fields = match self.peek() {
            Token::Star => Fields::All,
            Token::Word(w) if w == "KEY" => Fields::Key,
            Token::Word(w) if w == "VALUE" => Fields::Value,
            _ => return Err(self.error("'*', key or value")),
        };
        self.advance();
        Ok(fields)
    }

    /// `condition := key '=' string | key BETWEEN string AND string`
    fn condition(&mut self) -> Result<Condition, ParseError> {
        if !self.is_word("KEY") {
            // `value` is a field, but no condition on it is supported yet.
            return Err(self.error("key (conditions on the key only, so far)"));
        }
        self.advance();
        match self.peek() {
            Token::Equals => {
                self.advance();
                Ok(Condition::KeyEquals(self.expect_string()?))
            }
            Token::Word(w) if w == "BETWEEN" => {
                self.advance();
                let low = self.expect_string()?;
                self.expect_word("AND")?;
                let high = self.expect_string()?;
                Ok(Condition::KeyBetween(low, high))
            }
            _ => Err(self.error("'=' or BETWEEN")),
        }
    }
}

/// Parse one query.
pub fn parse(text: &str) -> Result<Select, ParseError> {
    Parser {
        tokens: tokenize(text)?,
        next: 0,
    }
    .select()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_star_is_both_fields_every_row() {
        assert_eq!(
            parse("SELECT *"),
            Ok(Select {
                distinct: false,
                fields: Fields::All,
                condition: None
            })
        );
    }

    #[test]
    fn spelling_does_not_reach_the_tree() {
        assert_eq!(
            parse("select   distinct VALUE"),
            parse("SELECT DISTINCT value")
        );
    }

    #[test]
    fn key_equals_and_key_between_parse_to_their_conditions() {
        assert_eq!(
            parse("SELECT value WHERE key = 'zx-spectrum'")
                .unwrap()
                .condition,
            Some(Condition::KeyEquals(b"zx-spectrum".to_vec()))
        );
        assert_eq!(
            parse("SELECT * WHERE key BETWEEN 'a' AND 'c'")
                .unwrap()
                .condition,
            Some(Condition::KeyBetween(b"a".to_vec(), b"c".to_vec()))
        );
    }

    #[test]
    fn a_doubled_quote_is_a_quote() {
        assert_eq!(
            parse("SELECT * WHERE key = 'it''s'")
                .unwrap()
                .condition,
            Some(Condition::KeyEquals(b"it's".to_vec()))
        );
    }

    #[test]
    fn display_spells_the_tree_back_as_sql_that_parses_to_it() {
        for text in [
            "SELECT *",
            "SELECT DISTINCT value",
            "SELECT key WHERE key = 'it''s'",
            "SELECT * WHERE key BETWEEN 'a' AND 'c'",
        ] {
            let tree = parse(text).unwrap();
            assert_eq!(tree.to_string(), text);
            assert_eq!(parse(&tree.to_string()), Ok(tree));
        }
    }

    #[test]
    fn errors_say_what_was_expected_and_where() {
        let cases = [
            (
                "SELECT",
                6,
                "expected '*', key or value, found the end of the query",
            ),
            (
                "SELECT * FROM t",
                9,
                "expected WHERE or the end, found FROM",
            ),
            ("SELECT * WHERE value = '1982'", 15, "expected key"),
            (
                "SELECT * WHERE key = zx",
                21,
                "expected a string in single quotes",
            ),
            ("SELECT * WHERE key BETWEEN 'a' 'c'", 31, "expected AND"),
            ("SELECT * WHERE key = 'open", 21, "string is never closed"),
            ("SELECT key;", 10, "unexpected ';'"),
        ];
        for (text, at, message) in cases {
            let err = parse(text).unwrap_err();
            assert_eq!(err.at, at, "{text:?}: {err}");
            assert!(
                err.message
                    .starts_with(message),
                "{text:?}: {err}"
            );
        }
    }
}
