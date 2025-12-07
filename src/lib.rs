use wasm_minimal_protocol::*;
use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use lindera::tokenizer::Tokenizer;
use std::sync::OnceLock;
use serde::{Deserialize, Serialize};

initiate_protocol!();

static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();

fn get_tokenizer() -> &'static Tokenizer {
    TOKENIZER.get_or_init(|| {
        let dictionary = load_dictionary("embedded://ipadic").expect("Failed to load dictionary");
        let segmenter = Segmenter::new(Mode::Normal, dictionary, None);
        Tokenizer::new(segmenter)
    })
}

#[derive(Deserialize)]
struct InputParams {
    text: String,
}

#[derive(Serialize)]
struct RubySegment {
    text: String,
    ruby: String,
}

#[derive(Serialize)]
struct TokenInfo {
    surface: String,
    pos: String,
    sub_pos: String,
    reading: String,
    base: String,
    ruby_segments: Vec<RubySegment>, 
}

fn hira_to_kata(c: char) -> char {
    if c >= '\u{3041}' && c <= '\u{3096}' {
        std::char::from_u32(c as u32 + 0x60).unwrap()
    } else {
        c
    }
}

fn build_ruby_segments(surface: &str, reading: &str) -> Vec<RubySegment> {
    if reading == "*" || surface == reading {
        return vec![RubySegment {
            text: surface.to_string(),
            ruby: "".to_string(),
        }];
    }

    let sur_chars: Vec<char> = surface.chars().collect();
    let read_chars: Vec<char> = reading.chars().collect();
    
    let mut segments = Vec::new();
    let mut buffer_s = String::new();
    let mut r_idx = 0;

    for &s_char in &sur_chars {
        let s_kata = hira_to_kata(s_char);
        let is_hiragana = s_char != s_kata;

        if is_hiragana {
            if r_idx < read_chars.len() {
                let remaining_reading = &read_chars[r_idx..];

                if let Some(pos_in_remaining) = remaining_reading.iter().position(|&c| c == s_kata) {
                    let kanji_reading_len = pos_in_remaining;
                    
                    if !buffer_s.is_empty() {
                        let end_idx = r_idx + kanji_reading_len;
                        if end_idx <= read_chars.len() {
                            let kanji_reading: String = read_chars[r_idx..end_idx].iter().collect();
                            segments.push(RubySegment {
                                text: buffer_s.clone(),
                                ruby: kanji_reading,
                            });
                        }
                        buffer_s.clear();
                    }

                    segments.push(RubySegment {
                        text: s_char.to_string(),
                        ruby: "".to_string(),
                    });

                    r_idx += kanji_reading_len + 1;
                    continue;
                }
            }
        }
        
        buffer_s.push(s_char);
    }

    if !buffer_s.is_empty() {
        let remaining_ruby: String = if r_idx < read_chars.len() {
            read_chars[r_idx..].iter().collect()
        } else {
            "".to_string()
        };
        segments.push(RubySegment {
            text: buffer_s,
            ruby: remaining_ruby,
        });
    }

    segments
}

#[wasm_func]
pub fn analyze(input_bytes: &[u8]) -> Vec<u8> {
    let params: InputParams = match serde_json::from_slice(input_bytes) {
        Ok(p) => p,
        Err(e) => return format!("Error: Invalid JSON: {}", e).into_bytes(),
    };

    let tokenizer = get_tokenizer();
    let mut tokens = match tokenizer.tokenize(&params.text) {
        Ok(t) => t,
        Err(e) => return format!("Error: Tokenization failed: {}", e).into_bytes(),
    };

    let result_list: Vec<TokenInfo> = tokens.iter_mut().map(|token| {
        let surface = token.surface.to_string();
        let details = token.details(); 
        let get_detail = |idx: usize| details.get(idx).map(|s| s.as_ref()).unwrap_or("*").to_string();
        
        let pos = get_detail(0);
        let reading = get_detail(7);

        let ruby_segments = build_ruby_segments(&surface, &reading);

        TokenInfo {
            surface,
            pos,
            sub_pos: get_detail(1),
            base: get_detail(6),
            reading,
            ruby_segments,
        }
    }).collect();

    match serde_json::to_vec(&result_list) {
        Ok(bytes) => bytes,
        Err(e) => format!("Error: Serialization failed: {}", e).into_bytes(),
    }
}