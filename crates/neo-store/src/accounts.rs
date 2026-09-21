use neo_core::{Allowance, ProviderAccount, ProviderAccountStatus, ProviderId};
use rusqlite::{OptionalExtension, params};

use crate::{ReadPool, Result, StoreError, Writer};

#[derive(Clone)]
pub struct ProviderAccountRepository {
    writer: Writer,
    readers: ReadPool,
}

impl ProviderAccountRepository {
    pub(crate) fn new(writer: Writer, readers: ReadPool) -> Self {
        Self { writer, readers }
    }

    pub fn get(&self, provider: &ProviderId) -> Result<Option<ProviderAccount>> {
        let provider = provider.as_str().to_owned();
        self.readers.read(move |connection| {
            let row = connection
                .query_row(
                    "SELECT provider, status, email, plan_type, workspace, allowance, updated_at FROM provider_accounts WHERE provider = ?1",
                    [provider],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()?;
            row.map(decode).transpose()
        })
    }

    pub fn put(&self, account: ProviderAccount) -> Result<()> {
        let provider = account.provider.as_str().to_owned();
        let status = encode_status(account.status);
        let allowance = account
            .allowance
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.writer.execute(move |connection| {
            connection.execute(
                "INSERT INTO provider_accounts(provider, status, email, plan_type, workspace, allowance, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(provider) DO UPDATE SET status=excluded.status, email=excluded.email, plan_type=excluded.plan_type, workspace=excluded.workspace, allowance=excluded.allowance, updated_at=excluded.updated_at",
                params![
                    provider,
                    status,
                    account.email,
                    account.plan_type,
                    account.workspace,
                    allowance,
                    account.updated_at
                ],
            )?;
            Ok(())
        })
    }
}

type AccountRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

fn decode(row: AccountRow) -> Result<ProviderAccount> {
    let allowance = row
        .5
        .map(|encoded| serde_json::from_str::<Allowance>(&encoded))
        .transpose()?;
    Ok(ProviderAccount {
        provider: ProviderId::new(row.0),
        status: decode_status(&row.1)?,
        email: row.2,
        plan_type: row.3,
        workspace: row.4,
        allowance,
        updated_at: row.6,
    })
}

fn encode_status(status: ProviderAccountStatus) -> &'static str {
    match status {
        ProviderAccountStatus::SignedOut => "signed_out",
        ProviderAccountStatus::Connected => "connected",
        ProviderAccountStatus::RateLimited => "rate_limited",
        ProviderAccountStatus::Unavailable => "unavailable",
    }
}

fn decode_status(value: &str) -> Result<ProviderAccountStatus> {
    match value {
        "signed_out" => Ok(ProviderAccountStatus::SignedOut),
        "connected" => Ok(ProviderAccountStatus::Connected),
        "rate_limited" => Ok(ProviderAccountStatus::RateLimited),
        "unavailable" => Ok(ProviderAccountStatus::Unavailable),
        other => Err(StoreError::InvalidProviderAccountStatus(other.to_owned())),
    }
}
