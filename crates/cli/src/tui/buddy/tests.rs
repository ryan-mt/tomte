use super::*;

#[test]
fn every_sprite_is_well_formed() {
    for pet in PETS {
        assert!(!pet.rows.is_empty(), "{} has no rows", pet.name);
        assert_eq!(pet.rows.len() % 2, 0, "{} needs even rows", pet.name);
        let w = pet.rows[0].chars().count();
        for row in pet.rows {
            assert_eq!(row.chars().count(), w, "{} row {row:?}", pet.name);
        }
        assert!(!pet.speech.is_empty(), "{} has no speech", pet.name);
    }
}

#[test]
fn weights_sum_to_one_hundred_and_every_tier_has_a_pet() {
    let total: f64 = RARITIES.iter().map(|r| r.weight()).sum();
    assert!((total - 100.0).abs() < 1e-9);
    for rarity in RARITIES {
        assert!(
            PETS.iter().any(|p| p.rarity == rarity),
            "no pet for {rarity:?}"
        );
    }
    // Exactly one legendary, the rarest.
    assert_eq!(
        PETS.iter()
            .filter(|p| p.rarity == Rarity::Legendary)
            .count(),
        1
    );
}

#[test]
fn rarity_weights_are_fixed() {
    // The drop rates are hardcoded and locked — changing any of these (or
    // their 100 total) is a deliberate act that must update this test.
    assert_eq!(Rarity::Common.weight(), 60.0);
    assert_eq!(Rarity::Uncommon.weight(), 25.0);
    assert_eq!(Rarity::Rare.weight(), 10.0);
    assert_eq!(Rarity::Epic.weight(), 4.0);
    assert_eq!(Rarity::Legendary.weight(), 1.0);
}

#[test]
fn roll_is_deterministic_for_an_identity() {
    assert_eq!(
        roll_weighted("anthropic-oauth:acct-123"),
        roll_weighted("anthropic-oauth:acct-123")
    );
}

#[test]
fn pick_from_tier_falls_back_on_empty_and_stays_in_bounds() {
    // An empty tier must not panic on `len() - 1` underflow.
    assert_eq!(pick_from_tier(&[], 0.0), 0);
    assert_eq!(pick_from_tier(&[], 0.999), 0);
    // Non-empty: r in [0, 1) maps across the tier and stays in bounds.
    assert_eq!(pick_from_tier(&[3, 5, 7], 0.0), 3);
    assert_eq!(pick_from_tier(&[3, 5, 7], 0.5), 5);
    assert_eq!(pick_from_tier(&[3, 5, 7], 0.999), 7);
}

#[test]
fn dev_override_parses_truthy_values() {
    use std::ffi::OsStr;
    assert!(dev_override(Some(OsStr::new("1"))));
    assert!(dev_override(Some(OsStr::new("yes"))));
    assert!(!dev_override(Some(OsStr::new("0"))));
    assert!(!dev_override(Some(OsStr::new("false"))));
    assert!(!dev_override(Some(OsStr::new("  "))));
    assert!(!dev_override(None));
}

#[test]
fn legendary_index_points_at_the_legendary() {
    assert_eq!(PETS[legendary_index()].rarity, Rarity::Legendary);
}

#[test]
fn roll_respects_weighted_rarity_distribution() {
    // Sample the seed space; common should dominate, legendary should be rare.
    let mut counts = [0usize; 5];
    for i in 0..20_000u32 {
        let idx = roll_weighted(&format!("session-{i}"));
        let rarity = PETS[idx].rarity;
        let slot = RARITIES.iter().position(|&r| r == rarity).unwrap();
        counts[slot] += 1;
    }
    // Common (60%) must be the most frequent; legendary (1%) the least.
    let common = counts[0];
    let legendary = counts[4];
    assert!(common > counts[1], "common should dominate: {counts:?}");
    assert!(legendary > 0, "legendary should still appear: {counts:?}");
    assert!(legendary < counts[3], "legendary rarest: {counts:?}");
}

/// Print each sprite as REAL 24-bit-colour half-block pixel art (the actual
/// render technique, not text), so the quality can be judged in a truecolor
/// terminal: `cargo test -p tomte buddy_colored_preview -- --ignored --nocapture`.
#[test]
#[ignore]
fn buddy_colored_preview() {
    for pet in PETS {
        println!("\n  {} [{}]", pet.name, pet.rarity.label());
        for pair in pet.rows.chunks(2) {
            let top: Vec<char> = pair[0].chars().collect();
            let bottom: Vec<char> = pair.get(1).copied().unwrap_or("").chars().collect();
            let w = top.len().max(bottom.len());
            let mut line = String::from("  ");
            for x in 0..w {
                let t = top.get(x).and_then(|&c| pet.color_of(c));
                let b = bottom.get(x).and_then(|&c| pet.color_of(c));
                match (t, b) {
                    (Some(t), Some(b)) => line.push_str(&format!(
                        "\x1b[38;2;{};{};{};48;2;{};{};{}m▀\x1b[0m",
                        t.0, t.1, t.2, b.0, b.1, b.2
                    )),
                    (Some(t), None) => {
                        line.push_str(&format!("\x1b[38;2;{};{};{}m▀\x1b[0m", t.0, t.1, t.2))
                    }
                    (None, Some(b)) => {
                        line.push_str(&format!("\x1b[38;2;{};{};{}m▄\x1b[0m", b.0, b.1, b.2))
                    }
                    (None, None) => line.push(' '),
                }
            }
            println!("{line}");
        }
    }
}

/// Print each sprite's silhouette so the shapes can be eyeballed with
/// `cargo test -p tomte buddy_silhouettes -- --nocapture --ignored`.
#[test]
#[ignore]
fn buddy_silhouettes() {
    for pet in PETS {
        println!("\n=== {} ({:?}) ===", pet.name, pet.rarity);
        for row in pet.rows {
            let line: String = row
                .chars()
                .map(|c| {
                    if pet.color_of(c).is_some() {
                        '█'
                    } else {
                        ' '
                    }
                })
                .collect();
            println!("{line}");
        }
    }
}
