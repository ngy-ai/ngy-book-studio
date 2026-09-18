//! Normalizing stored SQL so two definitions can be compared.

/// Tokenizes the stored DDL while preserving quoted values and token
/// boundaries. SQLite keeps the original spelling in `sqlite_schema`, so
/// keyword casing, comments and insignificant whitespace must not decide
/// compatibility. Keeping tokens separate also avoids treating `a TEXT` and
/// `at EXT` as the same definition after whitespace removal.
pub(crate) fn normalize_schema_sql(sql: &str) -> Vec<String> {
    let characters = sql.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut position = 0;

    while position < characters.len() {
        let character = characters[position];
        if character.is_whitespace() {
            position += 1;
            continue;
        }
        if character == '-' && characters.get(position + 1) == Some(&'-') {
            position += 2;
            while position < characters.len() && !matches!(characters[position], '\n' | '\r') {
                position += 1;
            }
            continue;
        }
        if character == '/' && characters.get(position + 1) == Some(&'*') {
            position += 2;
            while position + 1 < characters.len()
                && !(characters[position] == '*' && characters[position + 1] == '/')
            {
                position += 1;
            }
            position = (position + 2).min(characters.len());
            continue;
        }

        if matches!(character, '\'' | '"' | '`' | '[') {
            let quote_end = if character == '[' { ']' } else { character };
            let mut token = String::from(character);
            position += 1;
            while position < characters.len() {
                let quoted_character = characters[position];
                token.push(quoted_character);
                position += 1;
                if quoted_character == quote_end {
                    if quote_end != ']' && characters.get(position) == Some(&quote_end) {
                        token.push(characters[position]);
                        position += 1;
                    } else {
                        break;
                    }
                }
            }
            tokens.push(token);
            continue;
        }

        if is_schema_identifier_start(character) {
            let start = position;
            position += 1;
            while position < characters.len() && is_schema_identifier_continue(characters[position])
            {
                position += 1;
            }
            tokens.push(
                characters[start..position]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase(),
            );
            continue;
        }

        if character.is_ascii_digit()
            || (character == '.'
                && characters
                    .get(position + 1)
                    .is_some_and(char::is_ascii_digit))
        {
            let start = position;
            if character == '0'
                && characters
                    .get(position + 1)
                    .is_some_and(|next| matches!(next, 'x' | 'X'))
            {
                position += 2;
                while position < characters.len()
                    && (characters[position].is_ascii_hexdigit() || characters[position] == '_')
                {
                    position += 1;
                }
            } else {
                while position < characters.len()
                    && (characters[position].is_ascii_digit() || characters[position] == '_')
                {
                    position += 1;
                }
                if characters.get(position) == Some(&'.') {
                    position += 1;
                    while position < characters.len()
                        && (characters[position].is_ascii_digit() || characters[position] == '_')
                    {
                        position += 1;
                    }
                }
                if characters
                    .get(position)
                    .is_some_and(|next| matches!(next, 'e' | 'E'))
                {
                    position += 1;
                    if characters
                        .get(position)
                        .is_some_and(|next| matches!(next, '+' | '-'))
                    {
                        position += 1;
                    }
                    while position < characters.len()
                        && (characters[position].is_ascii_digit() || characters[position] == '_')
                    {
                        position += 1;
                    }
                }
            }
            tokens.push(
                characters[start..position]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase(),
            );
            continue;
        }

        let operator_width = if position + 2 < characters.len()
            && matches!(
                (
                    characters[position],
                    characters[position + 1],
                    characters[position + 2]
                ),
                ('-', '>', '>')
            ) {
            3
        } else if position + 1 < characters.len()
            && matches!(
                (characters[position], characters[position + 1]),
                ('|', '|')
                    | ('<', '<')
                    | ('>', '>')
                    | ('<', '=')
                    | ('>', '=')
                    | ('=', '=')
                    | ('!', '=')
                    | ('<', '>')
                    | ('-', '>')
            )
        {
            2
        } else {
            1
        };
        tokens.push(
            characters[position..position + operator_width]
                .iter()
                .collect(),
        );
        position += operator_width;
    }

    while tokens.last().is_some_and(|token| token == ";") {
        tokens.pop();
    }
    tokens
}

pub(crate) fn is_schema_identifier_start(character: char) -> bool {
    character == '_' || character == '$' || character.is_alphabetic()
}

pub(crate) fn is_schema_identifier_continue(character: char) -> bool {
    is_schema_identifier_start(character) || character.is_ascii_digit()
}
