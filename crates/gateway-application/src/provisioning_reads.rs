//! What onboarding has set up, read back without SQL.
//!
//! The admin command line lists merchants, their keys, their webhook
//! endpoints and the collectors money arrives at. Nothing here reveals a
//! secret: a key is shown by its prefix, an endpoint without its signing
//! fingerprint. Every list is bounded and continues from a cursor, so a
//! large deployment is read page by page instead of all at once.

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Clock, ProvisioningError, ProvisioningRepository, ProvisioningService, RandomBytes};

pub const DEFAULT_LIST_LIMIT: u32 = 50;
pub const MAX_LIST_LIMIT: u32 = 200;

/// One bounded page, continuing after `after` in id order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListRequest {
    pub limit: u32,
    pub after: Option<Uuid>,
}

impl ListRequest {
    /// # Errors
    ///
    /// Returns [`ProvisioningError::Invalid`] for a limit of zero or above the
    /// ceiling: a list that silently returns fewer rows than asked for reads
    /// as a complete one.
    pub fn new(limit: Option<u32>, after: Option<Uuid>) -> Result<Self, ProvisioningError> {
        let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT);
        if limit == 0 || limit > MAX_LIST_LIMIT {
            return Err(ProvisioningError::Invalid(
                "--limit must be between 1 and 200",
            ));
        }
        Ok(Self { limit, after })
    }
}

/// A page and the cursor to the next one, present only when a row past the
/// page was actually read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<Uuid>,
}

pub trait Listed {
    fn id(&self) -> Uuid;
}

impl<T: Listed> Listing<T> {
    fn from_rows(mut items: Vec<T>, limit: u32) -> Self {
        let has_more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = has_more.then(|| items.last().map(Listed::id)).flatten();
        Self { items, next_cursor }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerchantSummary {
    pub id: Uuid,
    pub external_id: String,
    pub display_name: String,
    pub status: String,
    pub collector_policy: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySummary {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub prefix: String,
    pub label: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookEndpointSummary {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub url: String,
    pub description: Option<String>,
    pub status: String,
    pub secret_version: i32,
    pub previous_secret_valid_until: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub disabled_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectorSummary {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub merchant_id: Option<Uuid>,
    pub address: String,
    pub state: String,
    /// Amount reservations that can still be paid; a collector is retired
    /// only once this is zero, unless it is compromised.
    pub open_reservations: i64,
    pub valid_from: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
}

macro_rules! listed {
    ($($name:ty),*) => {
        $(impl Listed for $name {
            fn id(&self) -> Uuid {
                self.id
            }
        })*
    };
}
listed!(
    MerchantSummary,
    ApiKeySummary,
    WebhookEndpointSummary,
    CollectorSummary
);

/// Each method returns up to `limit + 1` rows in ascending id order after the
/// cursor; the extra row is how the page knows another one exists.
#[async_trait]
pub trait ProvisioningReadRepository: Send + Sync {
    async fn list_merchants(
        &self,
        page: ListRequest,
    ) -> Result<Vec<MerchantSummary>, ProvisioningError>;

    async fn list_api_keys(
        &self,
        merchant_id: Uuid,
        page: ListRequest,
    ) -> Result<Vec<ApiKeySummary>, ProvisioningError>;

    async fn list_webhook_endpoints(
        &self,
        merchant_id: Uuid,
        page: ListRequest,
    ) -> Result<Vec<WebhookEndpointSummary>, ProvisioningError>;

    async fn list_collectors(
        &self,
        merchant_id: Option<Uuid>,
        page: ListRequest,
    ) -> Result<Vec<CollectorSummary>, ProvisioningError>;
}

impl<R, C, G> ProvisioningService<R, C, G>
where
    R: ProvisioningRepository + ProvisioningReadRepository,
    C: Clock,
    G: RandomBytes,
{
    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for an invalid page or a storage failure.
    pub async fn list_merchants(
        &self,
        page: ListRequest,
    ) -> Result<Listing<MerchantSummary>, ProvisioningError> {
        let rows = self.repository().list_merchants(page).await?;
        Ok(Listing::from_rows(rows, page.limit))
    }

    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for an invalid page or a storage failure.
    pub async fn list_api_keys(
        &self,
        merchant_id: Uuid,
        page: ListRequest,
    ) -> Result<Listing<ApiKeySummary>, ProvisioningError> {
        let rows = self.repository().list_api_keys(merchant_id, page).await?;
        Ok(Listing::from_rows(rows, page.limit))
    }

    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for an invalid page or a storage failure.
    pub async fn list_webhook_endpoints(
        &self,
        merchant_id: Uuid,
        page: ListRequest,
    ) -> Result<Listing<WebhookEndpointSummary>, ProvisioningError> {
        let rows = self
            .repository()
            .list_webhook_endpoints(merchant_id, page)
            .await?;
        Ok(Listing::from_rows(rows, page.limit))
    }

    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for an invalid page or a storage failure.
    pub async fn list_collectors(
        &self,
        merchant_id: Option<Uuid>,
        page: ListRequest,
    ) -> Result<Listing<CollectorSummary>, ProvisioningError> {
        let rows = self.repository().list_collectors(merchant_id, page).await?;
        Ok(Listing::from_rows(rows, page.limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merchant(id: u128) -> MerchantSummary {
        MerchantSummary {
            id: Uuid::from_u128(id),
            external_id: format!("m-{id}"),
            display_name: "Merchant".to_owned(),
            status: "active".to_owned(),
            collector_policy: "own".to_owned(),
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_cursor_is_given_only_when_another_row_was_read() {
        let full = Listing::from_rows(vec![merchant(1), merchant(2), merchant(3)], 2);
        assert_eq!(full.items.len(), 2);
        assert_eq!(full.next_cursor, Some(Uuid::from_u128(2)));
        let last = Listing::from_rows(vec![merchant(3)], 2);
        assert_eq!(last.next_cursor, None);
        let exact = Listing::from_rows(vec![merchant(1), merchant(2)], 2);
        assert_eq!(exact.next_cursor, None);
    }

    #[test]
    fn a_limit_outside_the_bounds_is_refused_not_clamped() {
        assert!(ListRequest::new(Some(0), None).is_err());
        assert!(ListRequest::new(Some(201), None).is_err());
        assert!(matches!(
            ListRequest::new(None, None),
            Ok(ListRequest { limit: 50, .. })
        ));
    }
}
