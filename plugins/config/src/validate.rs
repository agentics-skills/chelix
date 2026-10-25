//! Candidate checks. The only check today is that the text is a TOML table.

use std::future::Future;
use std::pin::Pin;

use futures::future::FutureExt;

pub struct Candidate {
    pub text: String,
    pub table: toml::Table,
    pub changed_sections: Vec<String>,
}

pub struct CheckError {
    pub name: &'static str,
    pub message: String,
}

pub trait CandidateCheck {
    fn name(&self) -> &'static str;
    fn check<'a>(&'a self, candidate: &'a Candidate) -> Pin<Box<dyn Future<Output = Result<(), CheckError>> + 'a>>;
}

pub struct TomlCheck;

impl CandidateCheck for TomlCheck {
    fn name(&self) -> &'static str {
        "toml"
    }

    fn check<'a>(&'a self, candidate: &'a Candidate) -> Pin<Box<dyn Future<Output = Result<(), CheckError>> + 'a>> {
        async move {
            toml::from_str::<toml::Table>(&candidate.text).map(|_| ()).map_err(|error| CheckError {
                name: self.name(),
                message: error.to_string(),
            })
        }
        .boxed_local()
    }
}

pub async fn run(checks: &[Box<dyn CandidateCheck>], candidate: &Candidate) -> Result<(), CheckError> {
    for check in checks {
        check.check(candidate).await?;
    }
    Ok(())
}
