use super::*;
use crate::{domain::InventoryView, registry::RegistryClient};

#[rocket::get("/images")]
pub fn page(_admin: AdminSession) -> Template {
    Template::render("images", context! {page:"images",title:"仓库镜像"})
}
#[rocket::get("/api/registries/<registry_id>/images?<repository>")]
pub async fn inventory(
    _admin: AdminSession,
    state: &State<AppState>,
    registry_id: &str,
    repository: Option<&str>,
) -> Result<Json<InventoryView>> {
    let _permit = state
        .inventory_gate
        .try_acquire()
        .map_err(|_| Error::RateLimited(5))?;
    let id = id(registry_id)?;
    let (registry, credentials, known) = db(state, move |s| s.registry_connection(id)).await?;
    let repository = repository.map(str::trim).filter(|v| !v.is_empty());
    Ok(Json(
        RegistryClient::new(&registry.endpoint, credentials)?
            .browse(&registry, &known, repository)
            .await?,
    ))
}
