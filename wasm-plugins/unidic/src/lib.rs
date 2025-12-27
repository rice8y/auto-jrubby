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
        load_dictionary("embedded://unidic").expect("Failed to load dictionary")
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

fn is_hiragana(c: char) -> bool {
    c >= '\u{3040}' && c <= '\u{309F}'
}

fn is_kanji(c: char) -> bool {
    // Common CJK range
    (c >= '\u{4E00}' && c <= '\u{9FFF}') ||
    // CJK Extension A
    (c >= '\u{3400}' && c <= '\u{4DBF}') ||
    // CJK Extension B
    (c >= '\u{20000}' && c <= '\u{2A6DF}')
}

fn contains_kanji(s: &str) -> bool {
    s.chars().any(is_kanji)
}

/// Reconstructs the orthographic reading (Standard Kana) from the Surface and Phonetic Reading.
/// 
/// This solves the alignment problem where the dictionary provides phonetic readings (using long vowels 'ー')
/// but the surface text uses standard orthography (using 'う', 'い', etc.).
/// 
/// Algorithm:
/// 1. Scan backwards from the end of both strings.
/// 2. If the Surface character is Kanji, stop scanning (trust the remaining Phonetic head).
/// 3. If Surface (Hiragana) matches Phonetic (Katakana) OR matches a phonetic long vowel 'ー',
///    adopt the Surface character (converted to Katakana) into the tail.
/// 4. If mismatch, break.
/// 5. Result = Remaining Phonetic Head + Reconstructed Tail.
fn reconstruct_orthography(surface: &str, phonetic: &str) -> String {
    let s_chars: Vec<char> = surface.chars().collect();
    let p_chars: Vec<char> = phonetic.chars().collect();

    let mut s_idx = s_chars.len() as isize - 1;
    let mut p_idx = p_chars.len() as isize - 1;
    
    let mut tail_orthography = String::new();

    while s_idx >= 0 && p_idx >= 0 {
        let s_char = s_chars[s_idx as usize];
        let p_char = p_chars[p_idx as usize];

        // Anchor: If we hit a Kanji in surface, we stop trusting the surface structure 
        // regarding reading, and trust the dictionary's remaining phonetic stem.
        if is_kanji(s_char) {
            break;
        }

        let s_kata = hira_to_kata(s_char);

        // Check compatibility
        let is_exact_match = s_kata == p_char;
        
        // Long vowel rule: Phonetic 'ー' can match Surface vowels (usually 'う' or 'い')
        // e.g., Surface 'う' matches Phonetic 'ー' in "行こう" (Ikou) vs "イコー" (Ikoo)
        let is_long_vowel_match = p_char == 'ー' && is_hiragana(s_char);

        if is_exact_match || is_long_vowel_match {
            // Adopt the Surface character (converted to Katakana) to preserve orthography
            // e.g., adopt 'ウ' instead of 'ー'
            tail_orthography.insert(0, s_kata);
            s_idx -= 1;
            p_idx -= 1;
        } else {
            // Mismatch (e.g., small tsu variations or completely different).
            // Stop reconstruction to be safe.
            break;
        }
    }

    // The head is whatever is left in the Phonetic string
    let head_phonetic: String = if p_idx >= 0 {
        p_chars[0..=(p_idx as usize)].iter().collect()
    } else {
        "".to_string()
    };

    format!("{}{}", head_phonetic, tail_orthography)
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

    let dummy_details = vec!["*".to_string(); 17];

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
        let details_vec: Vec<String> = token.details().iter().map(|s| s.to_string()).collect();
        
        // 1. If the word contains NO Kanji, do not generate ruby.
        //    (Fixes "コンピュータ", "123", punctuation issues)
        let ruby_segments = if !contains_kanji(&surface) {
            vec![RubySegment {
                text: surface.clone(),
                ruby: "".to_string(),
            }]
        } else {
            // 2. Determine base reading.
            //    We primarily want Index 9 (Phonological Surface / 発音形出現形) because it handles conjugations correctly.
            //    e.g. "し" -> "シ" (Index 9) vs "スル" (Index 6)
            //    e.g. "行こう" -> "イコー" (Index 9) vs "イク" (Index 6)
            let phonetic_idx = 9;
            
            // Fallback to Index 6 (Lemma) if Index 9 is missing
            let phonetic = details_vec.get(phonetic_idx)
                .filter(|s| s.as_str() != "*")
                .map(|s| s.as_str())
                .or_else(|| details_vec.get(6).map(|s| s.as_str()))
                .unwrap_or("*");

            // 3. Reconstruct Orthography
            //    Align "行こう" (Surface) with "イコー" (Phonetic) to get "イコウ" (Ideal Reading).
            let final_reading = if phonetic == "*" {
                "*".to_string()
            } else {
                reconstruct_orthography(&surface, phonetic)
            };

            build_ruby_segments(&surface, &final_reading)
        };

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