use std::fmt;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::AuthConfig;

const SCOPE_VERSION: &[u8] = b"ram-a-scope-v1\0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub tenant_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub permissions: Vec<String>,
}

impl Principal {
    pub fn scope_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(SCOPE_VERSION);
        update_tuple_field(&mut digest, &self.tenant_id);
        update_tuple_field(&mut digest, &self.user_id);
        format!("scope-{:x}", digest.finalize())
    }
}

struct TokenBinding {
    token_digest: [u8; 32],
    principal: Principal,
}

pub struct TokenAuthenticator {
    bindings: Vec<TokenBinding>,
}

impl TokenAuthenticator {
    pub fn from_config(config: &AuthConfig) -> Result<Self> {
        let mut bindings: Vec<TokenBinding> = Vec::with_capacity(config.tokens.len());

        for entry in &config.tokens {
            let Some(token) = std::env::var_os(&entry.token_env) else {
                bail!(
                    "token environment variable `{}` is unavailable",
                    entry.token_env
                );
            };
            let token = match token.into_string() {
                Ok(token) => token,
                Err(_) => bail!(
                    "token environment variable `{}` is not valid Unicode",
                    entry.token_env
                ),
            };
            if token.is_empty() {
                bail!("token environment variable `{}` is empty", entry.token_env);
            }

            let token_digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if bindings
                .iter()
                .any(|binding| bool::from(binding.token_digest.ct_eq(&token_digest)))
            {
                bail!("token environment variables resolve to duplicate values");
            }

            bindings.push(TokenBinding {
                token_digest,
                principal: Principal {
                    tenant_id: entry.tenant_id.clone(),
                    user_id: entry.user_id.clone(),
                    agent_id: entry.agent_id.clone(),
                    permissions: entry.permissions.clone(),
                },
            });
        }

        Ok(Self { bindings })
    }

    pub fn authenticate(&self, token: &str) -> Result<Principal> {
        self.authenticate_with_agent(token, None)
    }

    pub fn authenticate_with_agent(
        &self,
        token: &str,
        client_agent_id: Option<&str>,
    ) -> Result<Principal> {
        let candidate_digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut matched = None;

        for binding in &self.bindings {
            if bool::from(binding.token_digest.ct_eq(&candidate_digest)) {
                matched = Some(&binding.principal);
            }
        }

        let principal = matched.context("invalid authentication token")?;
        if client_agent_id.is_some_and(|agent_id| agent_id != principal.agent_id) {
            bail!("client agent ID does not match authenticated principal");
        }

        Ok(principal.clone())
    }
}

impl fmt::Debug for TokenAuthenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenAuthenticator")
            .field("binding_count", &self.bindings.len())
            .finish()
    }
}

fn update_tuple_field(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}
