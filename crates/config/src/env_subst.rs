/// Replace `${ENV_VAR}` placeholders using a custom lookup function.
///
/// Unresolvable variables are left as-is.
pub(crate) fn substitute_env_with(input: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    if !input.contains("${") {
        return input.to_string();
    }

    let mut result = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(relative_start) = input[cursor..].find("${") {
        let start = cursor + relative_start;
        result.push_str(&input[cursor..start]);

        let name_start = start + 2;
        let Some(relative_end) = input[name_start..].find('}') else {
            // Malformed — emit literal rest of input, matching the previous
            // char-by-char behavior that consumed the unterminated placeholder.
            result.push_str("${");
            result.push_str(&input[name_start..]);
            return result;
        };

        let end = name_start + relative_end;
        let var_name = &input[name_start..end];
        if var_name.is_empty() {
            // Malformed — emit literal without the consumed closing brace.
            result.push_str("${");
        } else if let Some(value) = lookup(var_name) {
            result.push_str(&value);
        } else {
            // Leave unresolved placeholder as-is.
            result.push_str("${");
            result.push_str(var_name);
            result.push('}');
        }

        cursor = end + 1;
    }

    result.push_str(&input[cursor..]);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_known_var() {
        let lookup = |name: &str| match name {
            "CHELIX_TEST_VAR" => Some("hello".to_string()),
            _ => None,
        };
        assert_eq!(
            substitute_env_with("key=${CHELIX_TEST_VAR}", lookup),
            "key=hello"
        );
    }

    #[test]
    fn leaves_unknown_var() {
        let lookup = |_: &str| None;
        assert_eq!(
            substitute_env_with("${CHELIX_NONEXISTENT_XYZ}", lookup),
            "${CHELIX_NONEXISTENT_XYZ}"
        );
    }

    #[test]
    fn no_placeholders() {
        assert_eq!(substitute_env_with("plain text", |_| None), "plain text");
    }
}
