use super::*;
use ratatui::style::Color;

#[test]
fn offset_color_preserves_non_rgb_and_clamps_each_rgb_channel() {
    for (input, delta, expected) in [
        (Color::Rgb(100, 100, 100), 10, Color::Rgb(110, 110, 110)),
        (Color::Rgb(100, 100, 100), -50, Color::Rgb(50, 50, 50)),
        (Color::Rgb(42, 42, 42), 0, Color::Rgb(42, 42, 42)),
        (Color::Rgb(250, 250, 250), 20, Color::Rgb(255, 255, 255)),
        (Color::Rgb(5, 5, 5), -20, Color::Rgb(0, 0, 0)),
        (Color::Red, 10, Color::Red),
    ] {
        assert_eq!(
            offset_color(input, delta),
            expected,
            "{input:?}, delta={delta}"
        );
    }
}

#[test]
fn lerp_color_interpolates_clamped_rgb_and_uses_non_rgb_fallback() {
    for (start, end, amount, expected) in [
        (
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 255, 255),
            0.0,
            Color::Rgb(0, 0, 0),
        ),
        (
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 255, 255),
            1.0,
            Color::Rgb(255, 255, 255),
        ),
        (
            Color::Rgb(0, 0, 0),
            Color::Rgb(200, 200, 200),
            0.5,
            Color::Rgb(100, 100, 100),
        ),
        (
            Color::Rgb(0, 0, 0),
            Color::Rgb(200, 200, 200),
            0.25,
            Color::Rgb(50, 50, 50),
        ),
        (
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 255, 255),
            -0.5,
            Color::Rgb(0, 0, 0),
        ),
        (
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 255, 255),
            1.5,
            Color::Rgb(255, 255, 255),
        ),
        (Color::Red, Color::Blue, 0.3, Color::Red),
        (Color::Red, Color::Blue, 0.7, Color::Blue),
    ] {
        assert_eq!(
            lerp_color(start, end, amount),
            expected,
            "{start:?} to {end:?}, amount={amount}"
        );
    }
}

#[test]
fn theme_commands_encode_hyphens_and_roundtrip_config_keys() {
    for (key, command) in [
        ("dark", "theme_dark"),
        ("one-dark-pro", "theme_one_dark_pro"),
        ("gruvbox-dark", "theme_gruvbox_dark"),
    ] {
        assert_eq!(theme_command_id(key), command);
        assert_eq!(config_key_from_command_id(command).as_deref(), Some(key));
    }
    assert!(config_key_from_command_id("invalid").is_none());
}

#[test]
fn hex_colors_accept_hash_case_and_vscode_shorthand_and_reject_invalid_length() {
    for (text, expected) in [
        ("#FF8000", Color::Rgb(255, 128, 0)),
        ("FF8000", Color::Rgb(255, 128, 0)),
        ("#ff0000", Color::Rgb(255, 0, 0)),
        ("#000000", Color::Rgb(0, 0, 0)),
        ("#FFF", Color::Rgb(255, 255, 255)),
        ("#1234", Color::Rgb(17, 34, 51)),
        ("#12", Color::Rgb(0, 0, 0)),
    ] {
        assert_eq!(hex_to_color(text), expected, "{text}");
    }
}

#[test]
fn color_hex_output_preserves_rgb_and_named_black_white() {
    let original = Color::Rgb(128, 64, 32);
    assert_eq!(hex_to_color(&color_to_hex(original)), original);
    for (color, expected) in [(Color::White, "#FFFFFF"), (Color::Black, "#000000")] {
        assert_eq!(color_to_hex(color), expected);
    }
}

#[test]
fn gradients_reach_horizontal_and_vertical_edges() {
    for (width, height, angle, first, last) in [
        (100, 50, 0.0, (0, 25), (99, 25)),
        (50, 100, 90.0, (25, 0), (25, 99)),
    ] {
        let gradient = GradientPrecomputed::new(width, height, angle);
        let start = gradient.t_at(first.0, first.1);
        let end = gradient.t_at(last.0, last.1);
        assert!(start < 0.01, "angle={angle}: start={start}");
        assert!(end > 0.99, "angle={angle}: end={end}");
    }
}

#[test]
fn diagonal_and_degenerate_gradient_coordinates_stay_clamped() {
    let gradient = GradientPrecomputed::new(10, 10, 45.0);
    for x in 0..10 {
        for y in 0..10 {
            let amount = gradient.t_at(x, y);
            assert!((0.0..=1.0).contains(&amount), "t_at({x},{y})={amount}");
        }
    }
    assert_eq!(GradientPrecomputed::new(1, 1, 0.0).t_at(0, 0), 0.0);
}
