#![forbid(unsafe_code)]

use std::{fs, path::Path};

use cssparser::{
    BasicParseErrorKind, Delimiter, ParseError, Parser, ParserInput, Token,
    color::parse_named_color,
};

const COMPONENT_STYLES: [&str; 9] = [
    "anchor",
    "button",
    "collapsible",
    "dialog",
    "field",
    "menu",
    "spinner",
    "status",
    "tabs",
];

#[test]
fn built_in_component_css_contains_no_literal_colors() {
    let styles = Path::new(env!("CARGO_MANIFEST_DIR")).join("registry/styles");
    for name in COMPONENT_STYLES {
        let path = styles.join(format!("{name}.css"));
        let css = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let violations = audit_stylesheet(&css);
        assert!(
            violations.is_empty(),
            "{} contains literal theme colors:\n{}",
            path.display(),
            violations.join("\n")
        );
    }
}

#[test]
fn parser_rejects_every_literal_color_form_and_nested_literal() {
    for (label, value) in [
        ("named", "red"),
        ("mixed-case named", "ReBeCcAPuRpLe"),
        ("system", "CanvasText"),
        ("legacy system", "ActiveBorder"),
        ("short hex", "#fff"),
        ("alpha hex", "#112233cc"),
        ("legacy rgb", "rgb(1, 2, 3)"),
        ("modern rgb", "rgb(1 2 3 / 50%)"),
        ("hsl", "hsl(120 50% 50%)"),
        ("hwb", "hwb(120 10% 20%)"),
        ("lab", "lab(50% 0 0)"),
        ("lch", "lch(50% 20 30)"),
        ("oklab", "oklab(50% 0 0)"),
        ("oklch", "oklch(50% 0.2 30)"),
        ("color", "color(display-p3 1 0 0)"),
        ("device cmyk", "device-cmyk(0 1 1 0)"),
        (
            "gradient literal",
            "linear-gradient(var(--kit-color-surface), red)",
        ),
        (
            "nested color mix literal",
            "color-mix(in srgb, var(--kit-color-surface), rgb(1 2 3))",
        ),
        ("variable literal fallback", "var(--app-color, #fff)"),
    ] {
        let css = format!(".component {{ color: {value}; }}");
        assert!(
            !audit_stylesheet(&css).is_empty(),
            "{label} literal was accepted: {value}"
        );
    }
}

#[test]
fn parser_allows_only_variable_current_transparent_and_inherited_colors() {
    for (label, value) in [
        ("variable", "var(--kit-color-text)"),
        ("current color", "currentColor"),
        ("transparent", "transparent"),
        ("inherit", "inherit"),
        (
            "variable color mix",
            "color-mix(in srgb, var(--kit-color-surface), var(--kit-color-text) 20%)",
        ),
        (
            "current color mix",
            "color-mix(in srgb, currentColor, transparent 30%)",
        ),
    ] {
        let css = format!(".component {{ color: {value}; }}");
        assert_eq!(audit_stylesheet(&css), Vec::<String>::new(), "{label}");
    }

    let comments_and_strings = r#"
        .component::before {
            content: "red #fff rgb(1 2 3)";
            /* color: rebeccapurple; */
            color: var(--kit-color-text);
        }
    "#;
    assert_eq!(audit_stylesheet(comments_and_strings), Vec::<String>::new());
}

#[test]
fn parser_fails_closed_for_malformed_color_syntax() {
    for css in [
        ".component { color: rgb(1 2 3; }",
        ".component { color: #12; }",
        ".component { color: \"unterminated\n; }",
    ] {
        assert!(
            !audit_stylesheet(css).is_empty(),
            "malformed CSS was accepted: {css}"
        );
    }
}

fn audit_stylesheet(css: &str) -> Vec<String> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut violations = Vec::new();
    audit_rule_tokens(&mut parser, &mut violations);
    violations
}

fn audit_rule_tokens<'i>(parser: &mut Parser<'i, '_>, violations: &mut Vec<String>) {
    loop {
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(error) if matches!(error.kind, BasicParseErrorKind::EndOfInput) => break,
            Err(error) => {
                violations.push(format!(
                    "CSS parse error at {:?}: {:?}",
                    error.location, error.kind
                ));
                break;
            }
        };

        match token {
            Token::Ident(property) => match parser.next() {
                Ok(Token::Colon) => {
                    let result: Result<(), ParseError<'i, ()>> =
                        parser.parse_until_before(Delimiter::Semicolon, |value| {
                            audit_value_tokens(value, &property, violations);
                            Ok(())
                        });
                    if let Err(error) = result {
                        violations.push(format!("invalid declaration value: {error:?}"));
                    }
                }
                Ok(next) => audit_rule_opening_token(next.clone(), parser, violations),
                Err(error) if matches!(error.kind, BasicParseErrorKind::EndOfInput) => {}
                Err(error) => violations.push(format!("invalid rule token: {error:?}")),
            },
            opening => audit_rule_opening_token(opening, parser, violations),
        }
    }
}

fn audit_rule_opening_token<'i>(
    token: Token<'i>,
    parser: &mut Parser<'i, '_>,
    violations: &mut Vec<String>,
) {
    if matches!(token, Token::CurlyBracketBlock) {
        let result: Result<(), ParseError<'i, ()>> = parser.parse_nested_block(|body| {
            audit_rule_tokens(body, violations);
            Ok(())
        });
        if let Err(error) = result {
            violations.push(format!("invalid rule block: {error:?}"));
        }
    }
}

fn audit_value_tokens<'i>(
    parser: &mut Parser<'i, '_>,
    property: &str,
    violations: &mut Vec<String>,
) {
    loop {
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(error) if matches!(error.kind, BasicParseErrorKind::EndOfInput) => break,
            Err(error) => {
                violations.push(format!(
                    "CSS value parse error at {:?}: {:?}",
                    error.location, error.kind
                ));
                break;
            }
        };

        match token {
            Token::Hash(value) | Token::IDHash(value) => {
                violations.push(format!("hex color #{value}"));
            }
            Token::Ident(value)
                if is_color_bearing_property(property)
                    && (parse_named_color(&value).is_ok() || is_system_color(&value)) =>
            {
                violations.push(format!("named or system color {value}"));
            }
            Token::Function(name) => {
                if is_literal_color_function(&name) {
                    violations.push(format!("literal color function {name}()"));
                }
                let result: Result<(), ParseError<'i, ()>> = parser.parse_nested_block(|nested| {
                    audit_value_tokens(nested, property, violations);
                    Ok(())
                });
                if let Err(error) = result {
                    violations.push(format!("invalid function {name}(): {error:?}"));
                }
            }
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                let result: Result<(), ParseError<'i, ()>> = parser.parse_nested_block(|nested| {
                    audit_value_tokens(nested, property, violations);
                    Ok(())
                });
                if let Err(error) = result {
                    violations.push(format!("invalid nested CSS value: {error:?}"));
                }
            }
            token if token.is_parse_error() => {
                violations.push(format!("invalid CSS value token: {token:?}"));
            }
            _ => {}
        }
    }
}

fn is_color_bearing_property(property: &str) -> bool {
    let property = property.to_ascii_lowercase();
    property.starts_with("--")
        || property.contains("color")
        || property.starts_with("background")
        || property.starts_with("border")
        || property.starts_with("outline")
        || property.starts_with("box-shadow")
        || property.starts_with("text-shadow")
        || property.starts_with("text-decoration")
        || property.starts_with("column-rule")
        || matches!(
            property.as_str(),
            "fill" | "stroke" | "filter" | "lighting-color" | "stop-color"
        )
}

fn is_literal_color_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "rgb"
            | "rgba"
            | "hsl"
            | "hsla"
            | "hwb"
            | "lab"
            | "lch"
            | "oklab"
            | "oklch"
            | "color"
            | "device-cmyk"
            | "light-dark"
    )
}

fn is_system_color(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "accentcolor"
            | "accentcolortext"
            | "activetext"
            | "buttonborder"
            | "buttonface"
            | "buttontext"
            | "canvas"
            | "canvastext"
            | "field"
            | "fieldtext"
            | "graytext"
            | "highlight"
            | "highlighttext"
            | "linktext"
            | "mark"
            | "marktext"
            | "selecteditem"
            | "selecteditemtext"
            | "visitedtext"
            | "activeborder"
            | "activecaption"
            | "appworkspace"
            | "background"
            | "buttonhighlight"
            | "buttonshadow"
            | "captiontext"
            | "inactiveborder"
            | "inactivecaption"
            | "inactivecaptiontext"
            | "infobackground"
            | "infotext"
            | "menu"
            | "menutext"
            | "scrollbar"
            | "threeddarkshadow"
            | "threedface"
            | "threedhighlight"
            | "threedlightshadow"
            | "threedshadow"
            | "window"
            | "windowframe"
            | "windowtext"
    )
}
