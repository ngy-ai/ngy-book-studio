//! The validation vocabulary: issues, the error that carries them and the
//! accumulator every check writes into.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

impl ValidationIssue {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationError {
    pub fn issues(&self) -> &[ValidationIssue] {
        &self.issues
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "document validation failed")?;
        for issue in &self.issues {
            write!(formatter, "; {}: {}", issue.path, issue.message)?;
        }
        Ok(())
    }
}

impl Error for ValidationError {}

#[derive(Default)]
pub(crate) struct Validator {
    pub(crate) issues: Vec<ValidationIssue>,
}

impl Validator {
    pub(crate) fn issue(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ValidationIssue::new(path, message));
    }

    pub(crate) fn finish(self) -> Result<(), ValidationError> {
        if self.issues.is_empty() {
            Ok(())
        } else {
            Err(ValidationError {
                issues: self.issues,
            })
        }
    }
}

pub(crate) fn validate_id(validator: &mut Validator, path: &str, id: &str) {
    if id.trim().is_empty() {
        validator.issue(path, "ID must not be empty");
    } else if id.trim() != id {
        validator.issue(path, "ID must not have leading or trailing whitespace");
    } else if id.chars().any(char::is_control) {
        validator.issue(path, "ID must not contain control characters");
    }
}

pub(crate) fn validate_required_text(validator: &mut Validator, path: &str, value: &str) {
    if value.trim().is_empty() {
        validator.issue(path, "value must not be blank");
    } else if value.chars().any(|character| character == '\0') {
        validator.issue(path, "value must not contain NUL");
    }
}

pub(crate) fn validate_optional_file_name(
    validator: &mut Validator,
    path: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        if value.trim().is_empty() {
            validator.issue(path, "file name must not be blank");
        } else if value.chars().any(|character| character == '\0') {
            validator.issue(path, "file name must not contain NUL");
        }
    }
}

pub(crate) fn validate_source_location_text(
    validator: &mut Validator,
    path: &str,
    value: &str,
    label: &str,
) {
    if value.trim().is_empty() {
        validator.issue(path, format!("{label} must not be blank"));
    } else if value.chars().any(char::is_control) {
        validator.issue(path, format!("{label} must not contain control characters"));
    }
}

pub(crate) fn validate_one_based(validator: &mut Validator, path: &str, value: u32, label: &str) {
    if value == 0 {
        validator.issue(path, format!("{label} is one-based and must be at least 1"));
    }
}
