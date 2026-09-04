pub(crate) fn is_type_case(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}
