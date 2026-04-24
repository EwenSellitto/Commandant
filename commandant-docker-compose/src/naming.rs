pub(crate) fn sanitize_hostname_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_dash = false;

    for character in value.chars().flat_map(|character| character.to_lowercase()) {
        let is_valid = character.is_ascii_lowercase() || character.is_ascii_digit();
        if is_valid {
            output.push(character);
            previous_dash = false;
        } else if !previous_dash {
            output.push('-');
            previous_dash = true;
        }
    }

    output.trim_matches('-').to_string()
}

pub(crate) fn sanitize_label_component(value: &str) -> String {
    sanitize_hostname_component(value).replace('-', "_")
}
