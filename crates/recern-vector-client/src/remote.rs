use crate::{
    Backend, Capabilities, HttpOptions,
    error::{Code, Error, Result},
    http::Transport,
    model,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct Remote {
    transport: Arc<Transport>,
    caps: Capabilities,
    identity: String,
    database_id: String,
    lease: Option<Arc<Lease>>,
}
struct Lease {
    transport: Arc<Transport>,
    id: String,
    closed: AtomicBool,
}
impl Lease {
    fn release(&self) -> Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let value = self.transport.request(
                "POST",
                "/v1/snapshots/release",
                Some(&json!({"snapshot_id":self.id})),
                true,
                false,
            )?;
            if value["released"].as_bool().is_none() {
                return Err(Error::corrupt());
            }
        }
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
impl Remote {
    pub fn connect(options: HttpOptions) -> Result<Self> {
        let mut transport = Transport::new(options, true)?;
        let info = transport.request("GET", "/v1/info", None, true, false)?;
        if info["api_version"].as_u64() != Some(1)
            || info["profile"].as_str() != Some(model::PROFILE)
        {
            return Err(Error::new(Code::Unsupported));
        }
        let caps: Capabilities = model::decode(info["capabilities"].clone())?;
        caps.validate()?;
        if caps.backend != "recern" || caps.snapshot_pages {
            return Err(Error::corrupt());
        }
        model::uint64(&info["sequence"])?;
        let database_id = info["database_id"]
            .as_str()
            .ok_or_else(Error::corrupt)?
            .to_owned();
        model::token(&database_id)?;
        for (field, target) in [
            ("max_request_bytes", &mut transport.request_limit),
            ("max_response_bytes", &mut transport.response_limit),
        ] {
            let limit = info["limits"][field].as_u64().ok_or_else(Error::corrupt)?;
            if !(4096..=model::MAX_HTTP_BYTES as u64).contains(&limit) {
                return Err(Error::corrupt());
            }
            *target = limit as usize;
        }
        let max_ms = info["limits"]["max_timeout_ms"]
            .as_u64()
            .filter(|v| (1..=30000).contains(v))
            .ok_or_else(Error::corrupt)?;
        transport.deadline_ms = transport.deadline_ms.min(max_ms);
        let identity = format!(
            "remote:{:x}",
            Sha256::digest(format!("{}\0{database_id}", transport.url))
        );
        Ok(Self {
            transport: Arc::new(transport),
            caps,
            identity,
            database_id,
            lease: None,
        })
    }
}
impl Backend for Remote {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn call(&self, route: &str, mut body: Value) -> Result<Value> {
        let write = matches!(
            route,
            "/v1/collections/create"
                | "/v1/collections/drop"
                | "/v1/records/upsert"
                | "/v1/records/delete"
        );
        if let Some(lease) = &self.lease {
            if lease.closed.load(Ordering::Acquire) {
                return Err(Error::unavailable());
            }
            if write {
                return Err(Error::new(Code::PermissionDenied));
            }
            body["snapshot_id"] = json!(lease.id);
        }
        let value = self
            .transport
            .request("POST", route, Some(&body), true, write)?;
        if !write {
            model::uint64(&value["sequence"])?;
        }
        Ok(value)
    }
    fn snapshot(&self) -> Result<Arc<dyn Backend>> {
        if self.lease.is_some() {
            return Err(Error::new(Code::Unsupported));
        }
        let value = self.transport.request(
            "POST",
            "/v1/snapshots/create",
            Some(&json!({})),
            true,
            false,
        )?;
        let id = value["snapshot_id"]
            .as_str()
            .ok_or_else(Error::corrupt)?
            .to_owned();
        model::token(&id)?;
        let lease = Arc::new(Lease {
            transport: self.transport.clone(),
            id,
            closed: AtomicBool::new(false),
        });
        model::uint64(&value["sequence"])?;
        let caps: Capabilities = model::decode(value["capabilities"].clone())?;
        caps.validate()?;
        if caps.backend != "recern"
            || !caps.snapshot_pages
            || value["database_id"] != self.database_id
        {
            return Err(Error::corrupt());
        }
        Ok(Arc::new(Self {
            transport: self.transport.clone(),
            caps,
            identity: self.identity.clone(),
            database_id: self.database_id.clone(),
            lease: Some(lease),
        }))
    }
    fn close(&self) -> Result<()> {
        self.lease.as_ref().map_or(Ok(()), |v| v.release())
    }
}
