use anyhow::Result;
use turbo_tasks::Value;
use turbopack_binding::turbopack::{
    core::{asset::AssetVc, reference_type::ReferenceType},
    turbopack::{
        transition::{ContextTransitionVc, Transition, TransitionVc},
        ModuleAssetContextVc,
    },
};

use super::NextDynamicEntryAssetVc;

/// This transition is used to create the marker asset for a next/dynamic
/// import. This will get picked up during module processing and will be used to
/// create the dynamic entry, and the dynamic manifest entry.
#[turbo_tasks::value]
pub struct NextDynamicTransition {
    client_transition: ContextTransitionVc,
}

#[turbo_tasks::value_impl]
impl NextDynamicTransitionVc {
    #[turbo_tasks::function]
    pub fn new(client_transition: ContextTransitionVc) -> Self {
        NextDynamicTransition { client_transition }.cell()
    }
}

#[turbo_tasks::value_impl]
impl Transition for NextDynamicTransition {
    #[turbo_tasks::function]
    async fn process(
        &self,
        asset: AssetVc,
        context: ModuleAssetContextVc,
        _reference_type: Value<ReferenceType>,
    ) -> Result<AssetVc> {
        let client_asset =
            self.client_transition
                .process(asset, context, Value::new(ReferenceType::Undefined));

        Ok(NextDynamicEntryAssetVc::new(client_asset).into())
    }
}
