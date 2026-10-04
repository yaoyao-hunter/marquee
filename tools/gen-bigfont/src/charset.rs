//! The V1 character set for the embedded bigfont asset
//! (docs/big-font-mode.md §3.2): printable ASCII, the GB2312 level 1+2
//! hanzi plus GB2312's own punctuation/fullwidth/kana zones, kana, and
//! fullwidth punctuation and forms.

use std::collections::BTreeSet;

/// Every character the zh-Hans asset should try to carry. The tool
/// intersects this with the glyphs actually present in the BDF.
pub fn charset_v1() -> BTreeSet<char> {
    let mut set = BTreeSet::new();
    set.extend(range(0x20, 0x7E)); // printable ASCII
    // GB2312 zones 1-5 (punctuation, numbers, fullwidth ASCII, kana) and
    // zones 16-87 (hanzi levels 1+2):
    set.extend(gb2312_zones());
    set.extend(range(0x3000, 0x303F)); // CJK symbols and punctuation (。、「」)
    set.extend(range(0x3041, 0x309F)); // hiragana
    set.extend(range(0x30A1, 0x30FF)); // katakana
    set.extend(range(0xFF01, 0xFF60)); // fullwidth forms (！～｠)
    set
}

fn range(lo: u32, hi: u32) -> impl Iterator<Item = char> {
    (lo..=hi).filter_map(char::from_u32)
}

/// The GB2312 cells of the zones the documented charset wants, decoded via
/// encoding_rs's GBK (which agrees with GB2312 on its defined cells).
///
/// Deliberately excluded: zones 6-9 (Greek, Cyrillic, pinyin marks, box
/// drawing — outside the documented charset) and zones 10-15 (undefined;
/// GBK best-fits them into the private use area, which must never enter
/// the asset).
pub fn gb2312_zones() -> impl Iterator<Item = char> {
    let leads = (0xA1u8..=0xA5).chain(0xB0..=0xF7);
    leads.flat_map(|lead| {
        (0xA1u8..=0xFE).filter_map(move |trail| {
            encoding_rs::GBK
                .decode_without_bom_handling_and_without_replacement(&[lead, trail])
                .and_then(|s| {
                    let mut chars = s.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => Some(c),
                        _ => None,
                    }
                })
                .filter(|c| !is_private_use(*c))
        })
    })
}

fn is_private_use(c: char) -> bool {
    ('\u{e000}'..='\u{f8ff}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gb2312_zones_cover_hanzi_and_punctuation() {
        let chars: BTreeSet<char> = gb2312_zones().collect();
        assert!(chars.contains(&'中')); // D6D0, level 1
        assert!(chars.contains(&'国')); // B9FA, level 2
        assert!(chars.contains(&'·')); // A1A4 interpunct
        assert!(chars.contains(&'—')); // A1AA em dash
        assert!(chars.contains(&'…')); // A1AD ellipsis
        assert!(chars.contains(&'①')); // A1BE circled one
        assert!(chars.contains(&'￥')); // A1EB fullwidth yen/yuan
        let hanzi = chars
            .iter()
            .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
            .count();
        assert_eq!(hanzi, 6763, "GB2312 levels 1+2 are exactly 6763 hanzi");
    }

    #[test]
    fn gb2312_zones_exclude_out_of_charset_cells() {
        let chars: BTreeSet<char> = gb2312_zones().collect();
        assert!(!chars.contains(&'働')); // Japanese-only hanzi: not in GB2312
        assert!(!chars.contains(&'込'));
        assert!(!chars.contains(&'α')); // zone 6 Greek
        assert!(!chars.contains(&'а')); // zone 7 Cyrillic
        assert!(!chars.contains(&'─')); // zone 9 box drawing
        assert!(
            !chars.iter().any(|c| is_private_use(*c)),
            "zones 10-15 GBK best-fits must not leak in"
        );
    }

    #[test]
    fn charset_covers_documented_ranges() {
        let set = charset_v1();
        for c in ['A', 'z', '0', ' ', '~'] {
            assert!(set.contains(&c), "missing ASCII {c:?}");
        }
        for c in ['あ', 'ん', 'ア', 'ヶ', 'ー'] {
            assert!(set.contains(&c), "missing kana {c:?}");
        }
        for c in ['。', '、', '「', '！', '￥', '｠'] {
            assert!(set.contains(&c), "missing fullwidth {c:?}");
        }
        assert!(!set.contains(&'\u{7f}'), "DEL must not be included");
        assert!(!set.contains(&'\u{1f}'));
        assert!(!set.contains(&'🚀'), "emoji are not in the V1 charset");
        assert!(
            !set.iter().any(|c| is_private_use(*c)),
            "no private use area"
        );
    }

    #[test]
    fn charset_size_stays_in_budget_band() {
        // ~95 ASCII + ~7150 GB2312 zone chars + the Unicode kana/fullwidth
        // blocks, minus overlaps → the asset budget (<250KB at 32 B per
        // glyph) assumes roughly this many glyphs.
        let n = charset_v1().len();
        assert!((7000..=7400).contains(&n), "got {n}");
    }
}
