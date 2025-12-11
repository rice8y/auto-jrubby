use wasm_minimal_protocol::*;
use lindera::dictionary::{load_dictionary, Dictionary, DictionaryBuilder, UserDictionaryLoader};
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use lindera::tokenizer::Tokenizer;
use std::sync::OnceLock;
use serde::{Deserialize, Serialize};

initiate_protocol!();

static DICTIONARY: OnceLock<Dictionary> = OnceLock::new();

fn get_dictionary() -> &'static Dictionary {
    DICTIONARY.get_or_init(|| {
        load_dictionary("embedded://ipadic").expect("Failed to load dictionary")
    })
}

#[derive(Deserialize)]
struct InputParams {
    text: String,
    #[serde(default)]
    user_dict_csv: Option<String>,
}

#[derive(Serialize)]
struct RubySegment {
    text: String,
    ruby: String,
}

#[derive(Serialize)]
struct TokenInfo {
    surface: String,
    details: Vec<String>, 
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

    let dictionary = get_dictionary().clone();

    let user_dictionary = if let Some(csv_data) = params.user_dict_csv {
        let builder = DictionaryBuilder::new(dictionary.metadata.clone());
        match UserDictionaryLoader::load_from_csv_data(builder, csv_data.as_bytes()) {
            Ok(ud) => Some(ud),
            Err(e) => return format!("Error: Failed to build user dictionary: {}", e).into_bytes(),
        }
    } else {
        None
    };

    let segmenter = Segmenter::new(Mode::Normal, dictionary, user_dictionary);
    let tokenizer = Tokenizer::new(segmenter);

    let mut tokens = match tokenizer.tokenize(&params.text) {
        Ok(t) => t,
        Err(e) => return format!("Error: Tokenization failed: {}", e).into_bytes(),
    };

    let mut result_list: Vec<TokenInfo> = Vec::new();
    let mut cursor_byte = 0;
    let text_bytes = params.text.as_bytes();

    // IPADICのフォーマットに合わせて空白用のダミー詳細を作成 ("*" で埋める)
    // IPADIC details: [品詞, 品詞細分類1, 品詞細分類2, 品詞細分類3, 活用形, 活用型, 原形, 読み, 発音] (計9個)
    let dummy_details = vec!["*".to_string(); 9];

    for token in tokens.iter_mut() {
        if token.byte_start > cursor_byte {
            let gap_slice = &text_bytes[cursor_byte..token.byte_start];
            let gap_text = String::from_utf8_lossy(gap_slice).to_string();
            
            let mut gap_details = dummy_details.clone();
            gap_details[0] = "Whitespace".to_string();

            result_list.push(TokenInfo {
                surface: gap_text.clone(),
                details: gap_details,
                ruby_segments: vec![RubySegment {
                    text: gap_text,
                    ruby: "".to_string(),
                }],
            });
        }

        let surface = token.surface.to_string();
        // token.details() はSurface以外のCSV列を返します
        let details_vec: Vec<String> = token.details().iter().map(|s| s.to_string()).collect();
        
        // IPADICの読み(Reading)はインデックス7
        let reading = details_vec.get(7).map(|s| s.as_str()).unwrap_or("*");

        let ruby_segments = build_ruby_segments(&surface, reading);

        result_list.push(TokenInfo {
            surface,
            details: details_vec,
            ruby_segments,
        });

        cursor_byte = token.byte_end;
    }

    if cursor_byte < text_bytes.len() {
        let gap_slice = &text_bytes[cursor_byte..];
        let gap_text = String::from_utf8_lossy(gap_slice).to_string();
        
        let mut gap_details = dummy_details.clone();
        gap_details[0] = "Whitespace".to_string();

        result_list.push(TokenInfo {
            surface: gap_text.clone(),
            details: gap_details,
            ruby_segments: vec![RubySegment {
                text: gap_text,
                ruby: "".to_string(),
            }],
        });
    }

    match serde_json::to_vec(&result_list) {
        Ok(bytes) => bytes,
        Err(e) => format!("Error: Serialization failed: {}", e).into_bytes(),
    }
}