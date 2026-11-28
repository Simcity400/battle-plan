use viewer_layers_forces::intel_icons::{Evaluate, ours};
use replay_data::rows::TableId;
use replay_frame::frame::DecodedFrame;
use viewer_foundation::perspective;
use viewer_raids::map::intel_icons::{self, IntelIcons};

impl Evaluate for crate::document::Content {
    fn row_tables(&self) -> &[TableId] {
        use TableId::*;
        &[
            AgencyOperationBuffers, Autonomy, CharacterOperatives, CharacterPortraits,
            CountryAppearance, CountryStabilityInputs, FactionMembers, IntelligenceAgencies,
            IntelNetworkAuxiliaries, IntelNetworks, LocalPlayer, ModifierDefinitions,
            Operations, OperativeMissions, PoliticalParties, Politics, RaidIntelInputs,
            StrategicOperatives, Subjects,
        ]
    }

    fn icons(&self, frame: &DecodedFrame, choice: &perspective::Choice) -> IntelIcons {
        match ours(frame, choice) {
            Ok(ours) => intel_icons::icons(&self.icons.intel, &self.icons.raids, frame, ours, &self.names.localisation, &self.raid_hover.recorded_language),
            Err(why) => refused(frame, &why.to_string()),
        }
    }
}

fn refused(frame: &DecodedFrame, why: &str) -> IntelIcons {
    let mut icons = IntelIcons { built_for: None, ..IntelIcons::default() };
    icons.report.reasons.insert(format!("the viewing country cannot be resolved at hour {}: {why}", frame.hour), 1);
    icons
}
