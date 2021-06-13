//! Provides things one needs for the electrical system of an aircraft.

mod battery;
mod battery_charge_limiter;
pub mod consumption;
mod emergency_generator;
mod engine_generator;
mod external_power_source;
mod static_inverter;
mod transformer_rectifier;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    time::Duration,
};

use crate::{
    shared::{
        ConsumePower, ElectricalBusType, ElectricalBuses, OptionIterator, PotentialOrigin,
        PowerConsumptionReport,
    },
    simulation::{
        SimulationElement, SimulationElementVisitor, SimulatorWriter, UpdateContext, Write,
    },
};
pub use battery::Battery;
pub use battery_charge_limiter::BatteryChargeLimiter;
pub use emergency_generator::EmergencyGenerator;
pub use engine_generator::{
    EngineGenerator, INTEGRATED_DRIVE_GENERATOR_STABILIZATION_TIME_IN_MILLISECONDS,
};
pub use external_power_source::ExternalPowerSource;
use itertools::Itertools;
pub use static_inverter::StaticInverter;
pub use transformer_rectifier::TransformerRectifier;
use uom::si::{electric_potential::volt, f64::*, power::watt, velocity::knot};
pub mod test;

pub trait AlternatingCurrentElectricalSystem {
    fn any_non_essential_bus_powered(&self, electricity: &Electricity) -> bool;
}

pub trait EngineGeneratorPushButtons {
    fn engine_gen_push_button_is_on(&self, number: usize) -> bool;
    fn idg_push_button_is_released(&self, number: usize) -> bool;
}

pub trait BatteryPushButtons {
    fn bat_is_auto(&self, number: usize) -> bool;
}

/// Within an electrical system, electric potential is made available by an origin.
/// These origins are contained in this type. By knowing the origin of potential
/// for all power consumers one can determine the amount of electric current provided
/// by the origin to the whole aircraft.
///
/// Note that this type shouldn't be confused with uom's `ElectricPotential`, which provides
/// the base unit (including volt) for defining the amount of potential.
/// The raw `ElectricPotential` is included in this type.
///
/// The raw `ElectricPotential` is ignored when determining if the potential
/// is powered or not. If we wouldn't ignore it, passing electric potential across the
/// circuit would take multiple simulation ticks, as _V_ for origins (ENG GEN, TR, etc)
/// can only be calculated when electrical consumption is known, which is the case at
/// the end of a simulation tick.
///
/// As the raw `ElectricPotential` is of less importance for the majority of code,
/// it is not taken into account when checking for partial equality.
///
/// For the reasons outlined above when creating e.g. an engine generator, ensure you
/// return `Potential::none()` when the generator isn't supplying potential, and
/// `Potential::some(PotentialOrigin::EngineGenerator(1), ElectricPotential::new::<volt>(115.))`
/// when it is.
#[derive(Clone, Copy, Debug)]
pub struct Potential {
    // As this struct is passed around quite a bit, we use a fixed sized
    // array so copying is cheaper. Creation of Potential, with two merges
    // and a clone is much cheaper: 286ns instead of 500ns with a HashSet.
    // Three elements is the maximum we expect in the A320 (BAT1, BAT2,
    // and TR1 or TR2). Should another aircraft require more one can simply
    // increase the number here and in the code below.
    origins: [Option<PotentialOrigin>; 3],
    raw: ElectricPotential,
}
impl Potential {
    pub fn none() -> Self {
        Self {
            origins: [None, None, None],
            raw: ElectricPotential::new::<volt>(0.),
        }
    }

    pub fn single(origin: PotentialOrigin, raw: ElectricPotential) -> Self {
        Self {
            origins: [Some(origin), None, None],
            raw,
        }
    }

    pub fn raw(&self) -> ElectricPotential {
        self.raw
    }

    pub fn count(&self) -> usize {
        self.origins().count()
    }

    pub fn origins(&self) -> impl Iterator<Item = PotentialOrigin> + '_ {
        self.origins.iter().filter_map(|&x| x)
    }

    pub fn merge(&self, other: &Potential) -> Self {
        // As a given simulation tick is not of infinitely small delta time. We need to give
        // "equality" some slack. This prevents continuously switching between potential
        // sources, such as the battery.
        if (self.raw - other.raw).abs() <= ElectricPotential::new::<volt>(0.001) {
            let mut elements = self.origins().chain(other.origins()).unique();

            let merged = Self {
                origins: [elements.next(), elements.next(), elements.next()],
                // Here we take the average of the potentials. To understand why consider
                // two batteries providing potential. BAT1 at 27.05V and BAT2 at 27.1V.
                // If we would return the higher potential, BAT1 would start charging itself.
                raw: self.raw.min(other.raw),
            };

            debug_assert!(
                elements.count() == 0,
                "No more elements expected. Consider increasing the size
                of the origins array if more than {} elements are to be expected.",
                self.origins.len()
            );

            merged
        } else if self.raw > other.raw {
            *self
        } else {
            *other
        }
    }

    pub fn is_single(&self, origin: PotentialOrigin) -> bool {
        match self.origins {
            [Some(x), None, None] => x == origin,
            _ => false,
        }
    }

    pub fn is_single_engine_generator(&self) -> bool {
        matches!(
            self.origins,
            [Some(PotentialOrigin::EngineGenerator(_)), None, None]
        )
    }

    pub fn is_pair(&self, left: PotentialOrigin, right: PotentialOrigin) -> bool {
        match self.origins {
            [Some(first), Some(second), None] => {
                (first == left && second == right) || (first == right && second == left)
            }
            _ => false,
        }
    }

    /// Indicates if the instance provides electric potential.
    pub fn is_powered(&self) -> bool {
        matches!(self.origins.first(), Some(Some(_)))
    }

    /// Indicates if the instance does not provide electric potential.
    pub fn is_unpowered(&self) -> bool {
        !self.is_powered()
    }
}
impl PartialEq for Potential {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl PartialOrd for Potential {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.raw.partial_cmp(&other.raw)
    }
}
impl PotentialSource for Potential {
    fn output(&self) -> Potential {
        *self
    }
}
impl Default for Potential {
    fn default() -> Self {
        Potential::none()
    }
}

/// A source of electric potential. A source is not necessarily the
/// origin of the potential. It can also be a conductor.
pub trait PotentialSource {
    fn output(&self) -> Potential;

    fn potential(&self) -> ElectricPotential {
        self.output().raw()
    }

    /// Indicates if the instance provides electric potential.
    fn is_powered(&self) -> bool {
        self.output().is_powered()
    }

    /// Indicates if the instance does not provide electric potential.
    fn is_unpowered(&self) -> bool {
        self.output().is_unpowered()
    }
}

/// A target for electric potential.
///
/// # Examples
///
/// To implement this trait, use the `potential_target!` macro when working within
/// the systems crate. When adding a new type outside of the crate, use the example
/// below:
/// ```rust
/// # use systems::electrical::{Potential, PotentialSource, PotentialTarget};
/// # struct MyType {
/// #     input_potential: Potential,
/// # }
/// impl PotentialTarget for MyType {
///     fn powered_by<T: PotentialSource + ?Sized>(&mut self, source: &T) {
///         self.input_potential = source.output();
///     }
///
///     fn or_powered_by<T: PotentialSource + ?Sized>(&mut self, source: &T) {
///         if self.input_potential.is_unpowered() {
///             self.powered_by(source);
///         }
///     }
/// }
/// ```
pub trait PotentialTarget {
    /// Powers the instance with the given source's potential. When the given source has no potential
    /// the instance also won't have potential.
    fn powered_by<T: PotentialSource + ?Sized>(&mut self, source: &T);

    /// Powers the instance with the given source's potential. When the given source has no potential
    /// the instance keeps its existing potential.
    fn or_powered_by<T: PotentialSource + ?Sized>(&mut self, source: &T);
}

/// Represents a contactor in an electrical power circuit.
/// When closed a contactor conducts the potential towards other targets.
#[derive(Debug)]
pub struct Contactor {
    identifier: ElectricalElementIdentifier,
    closed_id: String,
    closed: bool,
}
impl Contactor {
    pub fn new(
        id: &str,
        identifier_provider: &mut impl ElectricalElementIdentifierProvider,
    ) -> Contactor {
        Contactor {
            identifier: identifier_provider.next(),
            closed_id: format!("ELEC_CONTACTOR_{}_IS_CLOSED", id),
            closed: false,
        }
    }

    pub fn close_when(&mut self, should_be_closed: bool) {
        self.closed = should_be_closed;
    }

    pub fn is_open(&self) -> bool {
        !self.closed
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}
impl ElectricalElement for Contactor {
    fn input_identifier(&self) -> ElectricalElementIdentifier {
        self.identifier
    }

    fn output_identifier(&self) -> ElectricalElementIdentifier {
        self.identifier
    }

    fn is_conductive(&self) -> bool {
        self.closed
    }
}
impl SimulationElement for Contactor {
    fn write(&self, writer: &mut SimulatorWriter) {
        writer.write(&self.closed_id, self.is_closed());
    }
}

pub struct ElectricalBus {
    identifier: ElectricalElementIdentifier,
    bus_powered_id: String,
    bus_potential_normal_id: String,
    potential: ElectricPotential,
    bus_type: ElectricalBusType,
}
impl ElectricalBus {
    pub fn new(
        bus_type: ElectricalBusType,
        identifier_provider: &mut impl ElectricalElementIdentifierProvider,
    ) -> ElectricalBus {
        ElectricalBus {
            identifier: identifier_provider.next_for_bus(bus_type),
            bus_powered_id: format!("ELEC_{}_BUS_IS_POWERED", bus_type.to_string()),
            bus_potential_normal_id: format!("ELEC_{}_BUS_POTENTIAL_NORMAL", bus_type.to_string()),
            potential: ElectricPotential::new::<volt>(0.),
            bus_type,
        }
    }

    fn potential_normal(&self) -> bool {
        self.potential > ElectricPotential::new::<volt>(25.0)
    }
}
impl ElectricalElement for ElectricalBus {
    fn input_identifier(&self) -> ElectricalElementIdentifier {
        self.identifier
    }

    fn output_identifier(&self) -> ElectricalElementIdentifier {
        self.identifier
    }

    fn is_conductive(&self) -> bool {
        true
    }
}
impl SimulationElement for ElectricalBus {
    fn write(&self, writer: &mut SimulatorWriter) {
        if let ElectricalBusType::Sub(_) = self.bus_type {
            // Sub buses are not written towards the simulator. See the
            // description on the sub bus type for details.
            return;
        }

        writer.write(
            &self.bus_powered_id,
            self.potential > ElectricPotential::new::<volt>(0.),
        );
        if self.bus_type == ElectricalBusType::DirectCurrentBattery {
            // It's good to note that in the real aircraft, the battery charge limiters (BCLs) are
            // responsible for supplying this information to the SDAC. When the battery push
            // button is off the associated BCL is unpowered and thus not sending a signal to the SDAC.
            // If neither BCL sends signals to the SDAC this is translated into the amber XX you see
            // on the ECAM screen. For now we just always emit this information here and within
            // the ECAM code check the BAT push button position to see if XX should be presented or not.
            // Once the SDAC is implemented it can be moved there and read this value from the BCLs.
            writer.write(&self.bus_potential_normal_id, self.potential_normal())
        }
    }

    fn receive_power(&mut self, buses: &impl ElectricalBuses) {
        self.potential = buses.potential_of_bus(self.bus_type).raw();
    }
}

pub struct ElectricalStateWriter {
    current_id: String,
    current_normal_id: String,
    potential_id: String,
    potential_normal_id: String,
    frequency_id: String,
    frequency_normal_id: String,
    load_id: String,
    load_normal_id: String,
}
impl ElectricalStateWriter {
    pub fn new(element_id: &str) -> Self {
        Self {
            current_id: format!("ELEC_{}_CURRENT", element_id),
            current_normal_id: format!("ELEC_{}_CURRENT_NORMAL", element_id),
            potential_id: format!("ELEC_{}_POTENTIAL", element_id),
            potential_normal_id: format!("ELEC_{}_POTENTIAL_NORMAL", element_id),
            frequency_id: format!("ELEC_{}_FREQUENCY", element_id),
            frequency_normal_id: format!("ELEC_{}_FREQUENCY_NORMAL", element_id),
            load_id: format!("ELEC_{}_LOAD", element_id),
            load_normal_id: format!("ELEC_{}_LOAD_NORMAL", element_id),
        }
    }

    pub fn write_direct(
        &self,
        source: &(impl ProvideCurrent + ProvidePotential),
        writer: &mut SimulatorWriter,
    ) {
        self.write_current(source, writer);
        self.write_potential(source, writer);
    }

    pub fn write_alternating(
        &self,
        source: &(impl ProvidePotential + ProvideFrequency),
        writer: &mut SimulatorWriter,
    ) {
        self.write_potential(source, writer);
        self.write_frequency(source, writer);
    }

    pub fn write_alternating_with_load(
        &self,
        source: &(impl ProvidePotential + ProvideFrequency + ProvideLoad),
        writer: &mut SimulatorWriter,
    ) {
        self.write_alternating(source, writer);
        self.write_load(source, writer);
    }

    fn write_current(&self, source: &impl ProvideCurrent, writer: &mut SimulatorWriter) {
        writer.write(&self.current_id, source.current());
        writer.write(&self.current_normal_id, source.current_normal());
    }

    fn write_potential(&self, source: &impl ProvidePotential, writer: &mut SimulatorWriter) {
        writer.write(&self.potential_id, source.potential());
        writer.write(&self.potential_normal_id, source.potential_normal());
    }

    fn write_frequency(&self, source: &impl ProvideFrequency, writer: &mut SimulatorWriter) {
        writer.write(&self.frequency_id, source.frequency());
        writer.write(&self.frequency_normal_id, source.frequency_normal());
    }

    fn write_load(&self, source: &impl ProvideLoad, writer: &mut SimulatorWriter) {
        writer.write(&self.load_id, source.load());
        writer.write(&self.load_normal_id, source.load_normal());
    }
}

pub trait ProvideCurrent {
    fn current(&self) -> ElectricCurrent;
    fn current_normal(&self) -> bool;
}

pub trait ProvidePotential {
    fn potential(&self) -> ElectricPotential;
    fn potential_normal(&self) -> bool;
}

pub trait ProvideFrequency {
    fn frequency(&self) -> Frequency;
    fn frequency_normal(&self) -> bool;
}

pub trait ProvideLoad {
    fn load(&self) -> Ratio;
    fn load_normal(&self) -> bool;
}

/// Determines if and for how long the aircraft is in an emergency electrical situation.
pub struct EmergencyElectrical {
    is_active_for_duration: Duration,
}
impl EmergencyElectrical {
    pub fn new() -> Self {
        Self {
            is_active_for_duration: Duration::from_secs(0),
        }
    }

    pub fn update(
        &mut self,
        context: &UpdateContext,
        electricity: &Electricity,
        ac_electrical_system: &impl AlternatingCurrentElectricalSystem,
    ) {
        if !ac_electrical_system.any_non_essential_bus_powered(electricity)
            && context.indicated_airspeed() > Velocity::new::<knot>(100.)
        {
            self.is_active_for_duration += context.delta();
        } else {
            self.is_active_for_duration = Duration::from_secs(0)
        }
    }

    pub fn is_active(&self) -> bool {
        self.is_active_for_duration > Duration::from_secs(0)
    }

    fn active_duration(&self) -> Duration {
        self.is_active_for_duration
    }
}
impl Default for EmergencyElectrical {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uom::si::{electric_current::ampere, frequency::hertz, ratio::percent};

    struct StubElectricSource {}
    impl ProvideCurrent for StubElectricSource {
        fn current(&self) -> ElectricCurrent {
            ElectricCurrent::new::<ampere>(150.)
        }

        fn current_normal(&self) -> bool {
            true
        }
    }
    impl ProvidePotential for StubElectricSource {
        fn potential(&self) -> ElectricPotential {
            ElectricPotential::new::<volt>(28.)
        }

        fn potential_normal(&self) -> bool {
            true
        }
    }
    impl ProvideFrequency for StubElectricSource {
        fn frequency(&self) -> Frequency {
            Frequency::new::<hertz>(400.)
        }

        fn frequency_normal(&self) -> bool {
            true
        }
    }
    impl ProvideLoad for StubElectricSource {
        fn load(&self) -> Ratio {
            Ratio::new::<percent>(50.)
        }

        fn load_normal(&self) -> bool {
            true
        }
    }

    #[cfg(test)]
    mod potential_tests {
        use super::*;

        #[test]
        fn some_potential_is_powered() {
            assert_eq!(some_potential().is_powered(), true);
        }

        #[test]
        fn some_potential_is_not_unpowered() {
            assert_eq!(some_potential().is_unpowered(), false);
        }

        #[test]
        fn none_potential_is_not_powered() {
            assert_eq!(none_potential().is_powered(), false);
        }

        #[test]
        fn none_potential_is_unpowered() {
            assert_eq!(none_potential().is_unpowered(), true);
        }

        #[test]
        fn merge_ignores_none() {
            let potential = Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            )
            .merge(&Potential::none());

            assert!(potential.is_single(PotentialOrigin::ApuGenerator(1)));
            assert_eq!(potential.raw(), ElectricPotential::new::<volt>(115.));
        }

        #[test]
        fn merge_returns_the_callee_when_its_potential_is_greater_than_that_of_the_argument() {
            let potential = Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(40.),
            ));

            assert!(potential.is_single(PotentialOrigin::ApuGenerator(1)));
            assert_eq!(potential.raw(), ElectricPotential::new::<volt>(115.));
        }

        #[test]
        fn merge_returns_the_argument_when_its_potential_is_greater_than_that_of_the_callee() {
            let potential = Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(40.),
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.),
            ));

            assert!(potential.is_single(PotentialOrigin::EngineGenerator(1)));
            assert_eq!(potential.raw(), ElectricPotential::new::<volt>(115.));
        }

        #[test]
        fn merge_returns_a_merged_result_when_both_callee_and_argument_have_equal_potential() {
            let potential = Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.),
            ));

            assert!(potential.is_pair(
                PotentialOrigin::ApuGenerator(1),
                PotentialOrigin::EngineGenerator(1)
            ));
            assert_eq!(potential.raw(), ElectricPotential::new::<volt>(115.));
        }

        #[test]
        fn merge_combines_equal_origins() {
            let potential = Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            )
            .merge(&Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            ));

            assert!(potential.is_single(PotentialOrigin::ApuGenerator(1)));
            assert_eq!(potential.raw(), ElectricPotential::new::<volt>(115.));
        }

        #[test]
        fn merge_considers_miniscule_potential_differences_equal() {
            let potential = Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.0011),
            )
            .merge(&Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.002),
            ));

            assert!(potential.is_pair(
                PotentialOrigin::ApuGenerator(1),
                PotentialOrigin::EngineGenerator(1)
            ));
        }

        #[test]
        fn merge_considers_larger_potential_differences_inequal() {
            let potential = Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.001),
            )
            .merge(&Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.0021),
            ));

            assert!(potential.is_single(PotentialOrigin::ApuGenerator(1)));
        }

        #[test]
        fn merge_takes_the_lowest_raw_potential_from_two_potentials_it_considers_equal() {
            let potential = Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.0011),
            )
            .merge(&Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.002),
            ));

            assert_eq!(potential.raw(), ElectricPotential::new::<volt>(115.0011));
        }

        #[test]
        #[should_panic]
        fn merge_panics_when_merging_more_than_three_origins() {
            Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.),
            ))
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(2),
                ElectricPotential::new::<volt>(115.),
            ))
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(3),
                ElectricPotential::new::<volt>(115.),
            ));
        }

        #[test]
        fn count_returns_0_when_none() {
            assert_eq!(Potential::none().count(), 0);
        }

        #[test]
        fn count_returns_the_number_of_origins() {
            let potential = Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.),
            ));

            assert_eq!(potential.count(), 2);
        }

        #[test]
        fn is_single_returns_false_when_none() {
            assert!(!Potential::none().is_single(PotentialOrigin::External));
        }

        #[test]
        fn is_single_returns_false_when_single_of_different_origin() {
            assert!(!Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.)
            )
            .is_single(PotentialOrigin::External));
        }

        #[test]
        fn is_single_returns_true_when_single_of_given_origin() {
            assert!(Potential::single(
                PotentialOrigin::External,
                ElectricPotential::new::<volt>(115.)
            )
            .is_single(PotentialOrigin::External));
        }

        #[test]
        fn is_single_returns_false_when_pair() {
            assert!(!Potential::single(
                PotentialOrigin::External,
                ElectricPotential::new::<volt>(115.)
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.)
            ))
            .is_single(PotentialOrigin::External));
        }

        #[test]
        fn is_single_engine_generator_returns_false_when_none() {
            assert!(!Potential::none().is_single_engine_generator());
        }

        #[test]
        fn is_single_engine_generator_returns_false_when_different_origin() {
            assert!(!Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.)
            )
            .is_single_engine_generator());
        }

        #[test]
        fn is_single_engine_generator_returns_true_when_engine_generator() {
            assert!(Potential::single(
                PotentialOrigin::EngineGenerator(2),
                ElectricPotential::new::<volt>(115.)
            )
            .is_single_engine_generator());
        }

        #[test]
        fn is_single_engine_generator_returns_false_when_pair_of_engine_generators() {
            assert!(!Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.)
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(2),
                ElectricPotential::new::<volt>(115.)
            ))
            .is_single_engine_generator());
        }

        #[test]
        fn is_pair_returns_false_when_none() {
            assert!(!Potential::none().is_pair(
                PotentialOrigin::EngineGenerator(2),
                PotentialOrigin::EngineGenerator(1)
            ));
        }

        #[test]
        fn is_pair_returns_false_when_single() {
            assert!(!Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.)
            )
            .is_pair(
                PotentialOrigin::EngineGenerator(2),
                PotentialOrigin::EngineGenerator(1)
            ));
        }

        #[test]
        fn is_pair_returns_false_when_pair_with_different_origins() {
            assert!(!Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.)
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(2),
                ElectricPotential::new::<volt>(115.)
            ))
            .is_pair(
                PotentialOrigin::EngineGenerator(2),
                PotentialOrigin::EngineGenerator(1)
            ));
        }

        #[test]
        fn is_pair_returns_true_when_pair_of_given_origins_irregardless_of_order() {
            assert!(Potential::single(
                PotentialOrigin::EngineGenerator(1),
                ElectricPotential::new::<volt>(115.)
            )
            .merge(&Potential::single(
                PotentialOrigin::EngineGenerator(2),
                ElectricPotential::new::<volt>(115.)
            ))
            .is_pair(
                PotentialOrigin::EngineGenerator(2),
                PotentialOrigin::EngineGenerator(1)
            ));
        }

        fn some_potential() -> Potential {
            Potential::single(
                PotentialOrigin::ApuGenerator(1),
                ElectricPotential::new::<volt>(115.),
            )
        }

        fn none_potential() -> Potential {
            Potential::none()
        }
    }

    #[cfg(test)]
    mod electrical_bus_tests {
        use super::*;
        use crate::simulation::{test::SimulationTestBed, Aircraft};

        #[test]
        fn writes_its_state() {
            let mut test_bed = SimulationTestBed::new();
            let mut bus = electrical_bus(test_bed.electricity_mut());
            test_bed.run_without_update(&mut bus);

            assert!(test_bed.contains_key("ELEC_AC_2_BUS_IS_POWERED"));
        }

        #[test]
        fn sub_bus_does_not_write_its_state() {
            let mut test_bed = SimulationTestBed::new();
            let mut aircraft = ElectricalBusTestAircraft::new(
                ElectricalBusType::Sub("202PP"),
                test_bed.electricity_mut(),
            );
            test_bed.run_aircraft(&mut aircraft);

            assert!(!test_bed.contains_key("ELEC_SUB_202PP_BUS_IS_POWERED"));
        }

        struct BatteryStub {
            identifier: ElectricalElementIdentifier,
            potential: ElectricPotential,
        }
        impl BatteryStub {
            fn new(
                identifier_provider: &mut impl ElectricalElementIdentifierProvider,
            ) -> BatteryStub {
                BatteryStub {
                    identifier: identifier_provider.next(),
                    potential: ElectricPotential::new::<volt>(0.),
                }
            }

            fn set_potential(&mut self, potential: ElectricPotential) {
                self.potential = potential;
            }
        }
        impl ElectricalElement for BatteryStub {
            fn input_identifier(&self) -> ElectricalElementIdentifier {
                self.identifier
            }

            fn output_identifier(&self) -> ElectricalElementIdentifier {
                self.identifier
            }

            fn is_conductive(&self) -> bool {
                true
            }
        }
        impl ElectricitySource for BatteryStub {
            fn output_potential(&self) -> NewPotential {
                if self.potential > ElectricPotential::new::<volt>(0.) {
                    NewPotential::new(PotentialOrigin::Battery(1), self.potential)
                } else {
                    NewPotential::none()
                }
            }
        }

        struct ElectricalBusTestAircraft {
            bus: ElectricalBus,
            battery: BatteryStub,
        }
        impl ElectricalBusTestAircraft {
            fn new(bus_type: ElectricalBusType, electricity: &mut Electricity) -> Self {
                Self {
                    bus: ElectricalBus::new(bus_type, electricity),
                    battery: BatteryStub::new(electricity),
                }
            }

            fn powered_by_battery_at(&mut self, potential: ElectricPotential) {
                self.battery.set_potential(potential);
            }
        }
        impl Aircraft for ElectricalBusTestAircraft {
            fn update_before_power_distribution(
                &mut self,
                _: &UpdateContext,
                electricity: &mut Electricity,
            ) {
                electricity.supplied_by(&self.battery);
                electricity.flow(&self.battery, &self.bus);
            }
        }
        impl SimulationElement for ElectricalBusTestAircraft {
            fn accept<T: crate::simulation::SimulationElementVisitor>(&mut self, visitor: &mut T) {
                self.bus.accept(visitor);
                visitor.visit(self);
            }
        }

        #[test]
        fn bat_bus_at_25_volt_is_abnormal() {
            let mut test_bed = SimulationTestBed::new();
            let mut aircraft = ElectricalBusTestAircraft::new(
                ElectricalBusType::DirectCurrentBattery,
                test_bed.electricity_mut(),
            );

            aircraft.powered_by_battery_at(ElectricPotential::new::<volt>(25.));
            test_bed.run_aircraft(&mut aircraft);

            assert_eq!(
                test_bed.read_bool("ELEC_DC_BAT_BUS_POTENTIAL_NORMAL"),
                false
            );
        }

        #[test]
        fn bat_bus_above_25_volt_is_abnormal() {
            let mut test_bed = SimulationTestBed::new();
            let mut aircraft = ElectricalBusTestAircraft::new(
                ElectricalBusType::DirectCurrentBattery,
                test_bed.electricity_mut(),
            );

            aircraft.powered_by_battery_at(ElectricPotential::new::<volt>(25.01));
            test_bed.run_aircraft(&mut aircraft);

            assert_eq!(test_bed.read_bool("ELEC_DC_BAT_BUS_POTENTIAL_NORMAL"), true);
        }

        #[test]
        fn writes_potential_normal_when_bat_bus() {
            let mut electricity = Electricity::new();
            let mut bus =
                ElectricalBus::new(ElectricalBusType::DirectCurrentBattery, &mut electricity);

            let mut test_bed = SimulationTestBed::new();
            test_bed.run_without_update(&mut bus);

            assert!(test_bed.contains_key("ELEC_DC_BAT_BUS_POTENTIAL_NORMAL"));
        }

        #[test]
        fn does_not_write_potential_normal_when_not_bat_bus() {
            let mut electricity = Electricity::new();
            let mut bus = ElectricalBus::new(
                ElectricalBusType::AlternatingCurrentEssential,
                &mut electricity,
            );

            let mut test_bed = SimulationTestBed::new();
            test_bed.run_without_update(&mut bus);

            assert!(!test_bed.contains_key("ELEC_DC_BAT_BUS_POTENTIAL_NORMAL"));
        }

        fn electrical_bus(electricity: &mut Electricity) -> ElectricalBus {
            ElectricalBus::new(ElectricalBusType::AlternatingCurrent(2), electricity)
        }
    }

    #[cfg(test)]
    mod contactor_tests {
        use super::*;
        use crate::{electrical::test::TestElectricitySource, simulation::test::SimulationTestBed};

        #[test]
        fn contactor_starts_open() {
            assert!(contactor(&mut Electricity::new()).is_open());
        }

        #[test]
        fn open_contactor_when_toggled_open_stays_open() {
            let mut contactor = open_contactor(&mut Electricity::new());
            contactor.close_when(false);

            assert!(contactor.is_open());
        }

        #[test]
        fn open_contactor_when_toggled_closed_closes() {
            let mut contactor = open_contactor(&mut Electricity::new());
            contactor.close_when(true);

            assert!(contactor.is_closed());
        }

        #[test]
        fn closed_contactor_when_toggled_open_opens() {
            let mut contactor = closed_contactor(&mut Electricity::new());
            contactor.close_when(false);

            assert!(contactor.is_open());
        }

        #[test]
        fn closed_contactor_when_toggled_closed_stays_closed() {
            let mut contactor = closed_contactor(&mut Electricity::new());
            contactor.close_when(true);

            assert!(contactor.is_closed());
        }

        #[test]
        fn open_contactor_has_no_output_when_powered_by_nothing() {
            let mut electricity = Electricity::new();
            let contactor = open_contactor(&mut electricity);

            assert!(!electricity.is_powered(&contactor));
        }

        #[test]
        fn closed_contactor_has_no_output_when_powered_by_nothing() {
            let mut electricity = Electricity::new();
            let contactor = closed_contactor(&mut electricity);

            assert!(!electricity.is_powered(&contactor));
        }

        #[test]
        fn open_contactor_has_no_output_when_powered_by_nothing_which_is_powered() {
            let mut electricity = Electricity::new();
            contactor_has_no_output_when_powered_by_nothing_which_is_powered(
                open_contactor(&mut electricity),
                &mut electricity,
            );
        }

        #[test]
        fn closed_contactor_has_no_output_when_powered_by_nothing_which_is_powered() {
            let mut electricity = Electricity::new();
            contactor_has_no_output_when_powered_by_nothing_which_is_powered(
                closed_contactor(&mut electricity),
                &mut electricity,
            );
        }

        fn contactor_has_no_output_when_powered_by_nothing_which_is_powered(
            contactor: Contactor,
            electricity: &mut Electricity,
        ) {
            let unpowered =
                TestElectricitySource::unpowered(PotentialOrigin::External, electricity);
            electricity.supplied_by(&unpowered);

            assert!(!electricity.is_powered(&contactor));
        }

        #[test]
        fn open_contactor_has_no_output_when_powered_by_something() {
            let mut electricity = Electricity::new();
            let contactor = open_contactor(&mut electricity);
            let powered =
                TestElectricitySource::powered(PotentialOrigin::External, &mut electricity);
            electricity.supplied_by(&powered);

            assert!(!electricity.is_powered(&contactor));
        }

        #[test]
        fn closed_contactor_has_output_when_powered_by_something_which_is_powered() {
            let mut electricity = Electricity::new();
            let contactor = closed_contactor(&mut electricity);
            let powered =
                TestElectricitySource::powered(PotentialOrigin::External, &mut electricity);
            electricity.supplied_by(&powered);

            assert!(electricity.is_powered(&contactor));
        }

        #[test]
        fn writes_its_state() {
            let mut test_bed = SimulationTestBed::new();
            let mut contactor = contactor(test_bed.electricity_mut());
            test_bed.run_without_update(&mut contactor);

            assert!(test_bed.contains_key("ELEC_CONTACTOR_TEST_IS_CLOSED"));
        }

        fn contactor(electricity: &mut Electricity) -> Contactor {
            Contactor::new("TEST", electricity)
        }

        fn open_contactor(electricity: &mut Electricity) -> Contactor {
            let mut contactor = contactor(electricity);
            contactor.closed = false;

            contactor
        }

        fn closed_contactor(electricity: &mut Electricity) -> Contactor {
            let mut contactor = contactor(electricity);
            contactor.closed = true;

            contactor
        }
    }

    #[cfg(test)]
    mod current_state_writer_tests {
        use super::*;
        use crate::simulation::{test::SimulationTestBed, Aircraft};

        enum WriteType {
            DirectCurrent,
            AlternatingCurrent,
            AlternatingCurrentWithLoad,
        }

        struct CurrentStateWriterTestAircraft {
            write_type: WriteType,
            writer: ElectricalStateWriter,
        }
        impl CurrentStateWriterTestAircraft {
            fn new(write_type: WriteType) -> Self {
                Self {
                    write_type,
                    writer: ElectricalStateWriter::new("TEST"),
                }
            }
        }
        impl Aircraft for CurrentStateWriterTestAircraft {}
        impl SimulationElement for CurrentStateWriterTestAircraft {
            fn write(&self, writer: &mut SimulatorWriter) {
                match self.write_type {
                    WriteType::DirectCurrent => {
                        self.writer.write_direct(&StubElectricSource {}, writer)
                    }
                    WriteType::AlternatingCurrent => self
                        .writer
                        .write_alternating(&StubElectricSource {}, writer),
                    WriteType::AlternatingCurrentWithLoad => self
                        .writer
                        .write_alternating_with_load(&StubElectricSource {}, writer),
                }
            }
        }

        #[test]
        fn writes_direct_current_state() {
            let mut aircraft = CurrentStateWriterTestAircraft::new(WriteType::DirectCurrent);
            let mut test_bed = SimulationTestBed::new();

            test_bed.run_aircraft(&mut aircraft);

            assert!(test_bed.contains_key("ELEC_TEST_CURRENT"));
            assert!(test_bed.contains_key("ELEC_TEST_CURRENT_NORMAL"));
            assert!(test_bed.contains_key("ELEC_TEST_POTENTIAL"));
            assert!(test_bed.contains_key("ELEC_TEST_POTENTIAL_NORMAL"));
        }

        #[test]
        fn writes_alternating_current_state() {
            let mut aircraft = CurrentStateWriterTestAircraft::new(WriteType::AlternatingCurrent);
            let mut test_bed = SimulationTestBed::new();

            test_bed.run_aircraft(&mut aircraft);

            assert!(test_bed.contains_key("ELEC_TEST_POTENTIAL"));
            assert!(test_bed.contains_key("ELEC_TEST_POTENTIAL_NORMAL"));
            assert!(test_bed.contains_key("ELEC_TEST_FREQUENCY"));
            assert!(test_bed.contains_key("ELEC_TEST_FREQUENCY_NORMAL"));
        }

        #[test]
        fn writes_alternating_current_with_load_state() {
            let mut aircraft =
                CurrentStateWriterTestAircraft::new(WriteType::AlternatingCurrentWithLoad);
            let mut test_bed = SimulationTestBed::new();

            test_bed.run_aircraft(&mut aircraft);

            assert!(test_bed.contains_key("ELEC_TEST_POTENTIAL"));
            assert!(test_bed.contains_key("ELEC_TEST_POTENTIAL_NORMAL"));
            assert!(test_bed.contains_key("ELEC_TEST_FREQUENCY"));
            assert!(test_bed.contains_key("ELEC_TEST_FREQUENCY_NORMAL"));
            assert!(test_bed.contains_key("ELEC_TEST_LOAD"));
            assert!(test_bed.contains_key("ELEC_TEST_LOAD_NORMAL"));
        }
    }
}

pub trait ElectricalElement {
    fn input_identifier(&self) -> ElectricalElementIdentifier;
    fn output_identifier(&self) -> ElectricalElementIdentifier;

    /// Returns whether the element is currently capable of conducting electricity.
    fn is_conductive(&self) -> bool;
}

pub trait ElectricitySource: ElectricalElement {
    fn output_potential(&self) -> NewPotential;
}

pub trait ElectricityTransformer: ElectricalElement {
    fn transform(&self, input: &NewPotential) -> NewPotential;
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct ElectricalElementIdentifier(u32);
impl ElectricalElementIdentifier {
    fn first() -> Self {
        Self { 0: 1 }
    }

    fn next(&self) -> Self {
        Self { 0: self.0 + 1 }
    }
}

pub trait ElectricalElementIdentifierProvider {
    fn next(&mut self) -> ElectricalElementIdentifier;
    fn next_for_bus(&mut self, bus_type: ElectricalBusType) -> ElectricalElementIdentifier;
}

#[derive(Debug)]
pub struct Electricity {
    next_identifier: ElectricalElementIdentifier,
    buses: HashMap<ElectricalBusType, ElectricalElementIdentifier>,
    potential: PotentialCollection,
    none_potential: NewPotential,
    tick_delta: Duration,
}
impl Electricity {
    pub fn new() -> Self {
        Self {
            next_identifier: ElectricalElementIdentifier::first(),
            buses: HashMap::new(),
            potential: PotentialCollection::new(),
            none_potential: NewPotential::none(),
            tick_delta: Duration::from_secs(0),
        }
    }

    pub(super) fn pre_tick(&mut self, delta: Duration) {
        self.potential.clear();
        self.tick_delta = delta;
    }

    /// Flows electricity from the given output element to the given input element as long
    /// as both elements are conductive.
    ///
    /// For the vast majority of elements, the order of arguments is irrelevant, as their
    /// input and output shares the same identifier. However, transformers are split in two:
    /// the [TransformerRectifiers](TransformerRectifier) have AC input and DC output, and the [StaticInverter] has DC
    /// input and AC output. Thus for these specific element types, one has to make sure the
    /// argument order is correct.
    /// ```rust
    /// # use systems::{shared::ElectricalBusType, electrical::{Contactor, ElectricalBus, Electricity}};
    /// let mut electricity = Electricity::new();
    /// let contactor = Contactor::new("TEST", &mut electricity);
    /// let bus = ElectricalBus::new(ElectricalBusType::DirectCurrentBattery, &mut electricity);
    ///
    /// electricity.flow(&contactor, &bus);
    /// ```
    pub fn flow(
        &mut self,
        from_output: &impl ElectricalElement,
        to_input: &impl ElectricalElement,
    ) {
        if from_output.is_conductive() && to_input.is_conductive() {
            self.potential
                .flow(from_output.output_identifier(), to_input.input_identifier());
        }
    }

    /// Takes the output supplied by the given source of electricity, such that
    /// it can then [flow](`Self::flow()`) through the electrical system.
    /// ```rust
    /// # use systems::electrical::{Contactor, Electricity, EngineGenerator};
    /// let mut electricity = Electricity::new();
    /// let generator = EngineGenerator::new(1, &mut electricity);
    /// let contactor = Contactor::new("TEST", &mut electricity);
    ///
    /// electricity.supplied_by(&generator);
    /// electricity.flow(&generator, &contactor);
    /// ```
    pub fn supplied_by(&mut self, source: &impl ElectricitySource) {
        let output_identifier = source.output_identifier();
        self.potential.supplied_by(
            output_identifier,
            source.output_potential().include(output_identifier),
        )
    }

    /// Transforms electricity within the given transformer.
    /// ```rust
    /// # use systems::{shared::ElectricalBusType, electrical::{TransformerRectifier, ElectricalBus, Electricity}};
    /// let mut electricity = Electricity::new();
    /// let ac_bus = ElectricalBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);
    /// let tr = TransformerRectifier::new(1, &mut electricity);
    /// let dc_bus = ElectricalBus::new(ElectricalBusType::DirectCurrent(1), &mut electricity);
    ///
    /// electricity.flow(&ac_bus, &tr);
    /// electricity.transform_in(&tr);
    /// electricity.flow(&tr, &dc_bus);
    /// ```
    pub fn transform_in(&mut self, transformer: &impl ElectricityTransformer) {
        if let Some(input_potential) = self.potential.get(transformer.input_identifier()) {
            let output_identifier = transformer.output_identifier();
            let x = transformer
                .transform(input_potential)
                .include(output_identifier);
            self.potential.supplied_by(output_identifier, x);
        }
    }

    /// Returns if the given element is powered or not.
    fn is_powered(&self, element: &impl ElectricalElement) -> bool {
        self.potential.is_powered(element.output_identifier())
    }

    /// Returns if the given electrical bus type is powered or not.
    fn bus_is_powered(&self, bus_type: ElectricalBusType) -> bool {
        if let Some(identifier) = self.buses.get(&bus_type) {
            self.potential.is_powered(*identifier)
        } else {
            false
        }
    }

    pub fn output_of(&self, element: &impl ElectricalElement) -> &NewPotential {
        self.potential
            .get(element.output_identifier())
            .unwrap_or(&self.none_potential)
    }

    pub fn input_of(&self, element: &impl ElectricalElement) -> &NewPotential {
        self.potential
            .get(element.input_identifier())
            .unwrap_or(&self.none_potential)
    }

    pub fn is_powered_by(
        &self,
        element: &impl ElectricalElement,
    ) -> OptionIterator<impl Iterator<Item = &PotentialOrigin>> {
        OptionIterator::new(match self.potential.get(element.output_identifier()) {
            Some(potential) => Some(potential.origins()),
            None => None,
        })
    }

    pub fn distribute_to(&self, element: &mut impl SimulationElement, _: &UpdateContext) {
        let mut visitor = ReceivePowerVisitor::new(&self);
        element.accept(&mut visitor);
    }

    pub fn consume_in(&mut self, element: &mut impl SimulationElement) {
        let mut visitor = ConsumePowerVisitor::new(self);
        element.accept(&mut visitor);

        let mut visitor = ConsumePowerInConvertersVisitor::new(self);
        element.accept(&mut visitor);
    }

    pub fn report_consumption_to(&self, element: &mut impl SimulationElement) {
        let mut visitor = ProcessPowerConsumptionReportVisitor::new(&self);
        element.accept(&mut visitor);
    }

    #[cfg(test)]
    fn identifier_for(&self, bus_type: ElectricalBusType) -> Option<&ElectricalElementIdentifier> {
        self.buses.get(&bus_type)
    }
}
impl ElectricalElementIdentifierProvider for Electricity {
    fn next(&mut self) -> ElectricalElementIdentifier {
        let identifier = self.next_identifier;
        self.next_identifier = identifier.next();

        identifier
    }

    fn next_for_bus(&mut self, bus_type: ElectricalBusType) -> ElectricalElementIdentifier {
        let identifier = self.next();
        self.buses.insert(bus_type, identifier);

        identifier
    }
}
impl ElectricalBuses for Electricity {
    fn potential_of_bus(&self, bus_type: ElectricalBusType) -> &NewPotential {
        if let Some(identifier) = self.buses.get(&bus_type) {
            self.potential
                .get(*identifier)
                .unwrap_or(&self.none_potential)
        } else {
            &self.none_potential
        }
    }

    fn bus_is_powered(&self, bus_type: ElectricalBusType) -> bool {
        self.bus_is_powered(bus_type)
    }

    fn any_is_powered(&self, bus_types: &[ElectricalBusType]) -> bool {
        bus_types
            .iter()
            .any(|&bus_type| self.bus_is_powered(bus_type))
    }

    fn is_powered(&self, element: &impl ElectricalElement) -> bool {
        self.is_powered(element)
    }
}
impl ConsumePower for Electricity {
    fn input_of(&self, element: &impl ElectricalElement) -> &NewPotential {
        self.input_of(element)
    }

    fn consume_from_input(&mut self, element: &impl ElectricalElement, power: Power) {
        self.potential
            .consume_from(element.input_identifier(), power);
    }

    fn consume_from_bus(&mut self, bus_type: ElectricalBusType, power: Power) {
        if let Some(identifier) = self.buses.get(&bus_type) {
            self.potential.consume_from(*identifier, power);
        }
    }
}
impl PowerConsumptionReport for Electricity {
    fn total_consumption_of(&self, origin: PotentialOrigin) -> Power {
        self.potential.total_consumption_of(origin)
    }

    fn is_powered(&self, element: &impl ElectricalElement) -> bool {
        self.is_powered(element)
    }

    fn delta(&self) -> Duration {
        self.tick_delta
    }
}

struct ReceivePowerVisitor<'a> {
    electricity: &'a Electricity,
}
impl<'a> ReceivePowerVisitor<'a> {
    pub fn new(electricity: &'a Electricity) -> Self {
        ReceivePowerVisitor { electricity }
    }
}
impl<'a> SimulationElementVisitor for ReceivePowerVisitor<'a> {
    fn visit<T: SimulationElement>(&mut self, visited: &mut T) {
        visited.receive_power(self.electricity);
    }
}

struct ConsumePowerVisitor<'a> {
    electricity: &'a mut Electricity,
}
impl<'a> ConsumePowerVisitor<'a> {
    pub fn new(electricity: &'a mut Electricity) -> Self {
        ConsumePowerVisitor { electricity }
    }
}
impl<'a> SimulationElementVisitor for ConsumePowerVisitor<'a> {
    fn visit<T: SimulationElement>(&mut self, visited: &mut T) {
        visited.consume_power(self.electricity);
    }
}
struct ConsumePowerInConvertersVisitor<'a> {
    electricity: &'a mut Electricity,
}
impl<'a> ConsumePowerInConvertersVisitor<'a> {
    pub fn new(electricity: &'a mut Electricity) -> Self {
        ConsumePowerInConvertersVisitor { electricity }
    }
}
impl<'a> SimulationElementVisitor for ConsumePowerInConvertersVisitor<'a> {
    fn visit<T: SimulationElement>(&mut self, visited: &mut T) {
        visited.consume_power_in_converters(self.electricity);
    }
}

struct ProcessPowerConsumptionReportVisitor<'a> {
    electricity: &'a Electricity,
}
impl<'a> ProcessPowerConsumptionReportVisitor<'a> {
    pub fn new(electricity: &'a Electricity) -> Self {
        ProcessPowerConsumptionReportVisitor { electricity }
    }
}
impl<'a> SimulationElementVisitor for ProcessPowerConsumptionReportVisitor<'a> {
    fn visit<T: SimulationElement>(&mut self, visited: &mut T) {
        visited.process_power_consumption_report(self.electricity);
    }
}

#[derive(Debug)]
pub struct NewPotential {
    origins: HashSet<PotentialOrigin>,
    elements: HashSet<ElectricalElementIdentifier>,
    raw: ElectricPotential,
}
impl NewPotential {
    pub fn new(origin: PotentialOrigin, raw: ElectricPotential) -> Self {
        let mut origins = HashSet::new();
        origins.insert(origin);

        Self {
            origins,
            elements: HashSet::new(),
            raw,
        }
    }

    pub fn none() -> Self {
        Self {
            origins: HashSet::new(),
            elements: HashSet::new(),
            raw: ElectricPotential::new::<volt>(0.),
        }
    }

    pub fn raw(&self) -> ElectricPotential {
        self.raw
    }

    fn include(mut self, identifier: ElectricalElementIdentifier) -> Self {
        self.elements.insert(identifier);
        self
    }

    fn and_include(self, identifier: ElectricalElementIdentifier) -> Self {
        self.include(identifier)
    }

    fn elements(&self) -> impl Iterator<Item = &ElectricalElementIdentifier> + '_ {
        self.elements.iter()
    }

    fn origin_count(&self) -> usize {
        self.origins.iter().count()
    }

    fn origins(&self) -> impl Iterator<Item = &PotentialOrigin> + '_ {
        self.origins.iter()
    }

    fn merge(mut self, mut other: NewPotential) -> Self {
        // As a given simulation tick is not of infinitely small delta time. We need to give
        // "equality" some slack. This prevents continuously switching between potential
        // sources, such as the battery.
        if (self.raw - other.raw).abs() <= ElectricPotential::new::<volt>(0.001) {
            self.origins.extend(other.origins);
            self.elements.extend(other.elements.iter());

            // Here we take the minimum of the potentials. To understand why consider
            // two batteries providing potential. BAT1 at 27.05V and BAT2 at 27.1V.
            // If we would return the higher potential, BAT1 would start charging itself.
            self.raw = self.raw.min(other.raw);

            self
        } else if self.raw > other.raw {
            self.elements.extend(other.elements.iter());
            self
        } else {
            other.elements.extend(self.elements.iter());
            other
        }
    }

    pub fn is_powered(&self) -> bool {
        self.origins.len() > 0
    }

    pub fn is_unpowered(&self) -> bool {
        self.origins.len() == 0
    }

    pub fn is_only_powered_by_single_engine_generator(&self) -> bool {
        self.origins.len() == 1
            && matches!(
                self.origins.iter().next(),
                Some(PotentialOrigin::EngineGenerator(_))
            )
    }

    pub fn is_powered_by_same_single_source(&self, other: &NewPotential) -> bool {
        self.origins.len() == 1
            && other.origins.len() == 1
            && self.origins.iter().next() == other.origins.iter().next()
    }

    pub fn is_only_powered_by_apu(&self) -> bool {
        self.origins.len() == 1
            && matches!(
                self.origins.iter().next(),
                Some(PotentialOrigin::ApuGenerator(_))
            )
    }

    pub fn is_single(&self, origin: PotentialOrigin) -> bool {
        self.origins.len() == 1 && self.origins.contains(&origin)
    }

    pub fn is_pair(&self, x: PotentialOrigin, y: PotentialOrigin) -> bool {
        let mut set = HashSet::new();
        set.insert(x);
        set.insert(y);

        self.origins.symmetric_difference(&set).count() == 0
    }
}

// TODO: Remove this implementation once the migration towards the new electrical model is done.
impl Into<Potential> for &NewPotential {
    fn into(self) -> Potential {
        let mut potential = Potential::none();
        self.origins
            .iter()
            .for_each(|&origin| potential = potential.merge(&Potential::single(origin, self.raw)));

        potential
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PotentialKey(u32);
impl PotentialKey {
    fn first() -> Self {
        Self { 0: 1 }
    }

    fn next(&self) -> Self {
        Self { 0: self.0 + 1 }
    }
}

#[derive(Debug)]
struct ElectricalElementToPotentialKeyMap {
    next_key: PotentialKey,
    items: HashMap<ElectricalElementIdentifier, PotentialKey>,
}
impl ElectricalElementToPotentialKeyMap {
    fn new() -> Self {
        Self {
            next_key: PotentialKey::first(),
            items: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.next_key = PotentialKey::first();
        self.items.clear();
    }

    fn get(&self, id: ElectricalElementIdentifier) -> Option<PotentialKey> {
        match self.items.get(&id) {
            Some(key) => Some(*key),
            None => None,
        }
    }

    fn get_or_insert(&mut self, identifier: ElectricalElementIdentifier) -> PotentialKey {
        match self.items.get(&identifier) {
            Some(key) => *key,
            None => {
                let key = self.next_key;
                self.items.insert(identifier, self.next_key);
                self.next_key = self.next_key.next();

                key
            }
        }
    }

    fn insert(&mut self, id: ElectricalElementIdentifier, key: PotentialKey) {
        self.items.insert(id, key);
    }
}

/// Maintains the many to one relationship from electrical elements to their electric potential.
#[derive(Debug)]
struct PotentialCollection {
    element_to_potential_key: ElectricalElementToPotentialKeyMap,
    items: HashMap<PotentialKey, NewPotential>,
    consumption_per_origin: HashMap<PotentialOrigin, Power>,
}
impl PotentialCollection {
    fn new() -> Self {
        Self {
            element_to_potential_key: ElectricalElementToPotentialKeyMap::new(),
            items: HashMap::new(),
            consumption_per_origin: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.element_to_potential_key.clear();
        self.items.clear();
        self.consumption_per_origin.clear();
    }

    fn flow(
        &mut self,
        left_element: ElectricalElementIdentifier,
        right_element: ElectricalElementIdentifier,
    ) {
        let left_key = self.element_to_potential_key.get_or_insert(left_element);
        let right_key = self.element_to_potential_key.get_or_insert(right_element);
        match (self.items.remove(&left_key), self.items.remove(&right_key)) {
            (None, None) => {
                // Neither element has potential, point them both to an object without potential.
                self.items.insert(
                    left_key,
                    NewPotential::none()
                        .include(left_element)
                        .and_include(right_element),
                );
                self.element_to_potential_key
                    .insert(right_element, left_key);
            }
            (Some(left_potential), None) => {
                // The right element doesn't yet have potential, point it to the left element's potential.
                self.element_to_potential_key
                    .insert(right_element, left_key);
                self.items
                    .insert(left_key, left_potential.include(right_element));
            }
            (None, Some(right_potential)) => {
                // The left element doesn't yet have potential, point it to the right element's potential.
                self.element_to_potential_key
                    .insert(left_element, right_key);
                self.items
                    .insert(right_key, right_potential.include(left_element));
            }
            (Some(left_potential), Some(right_potential)) => {
                // The right element's potential will merge into the left element's potential.
                for right_element in right_potential.elements() {
                    self.element_to_potential_key
                        .insert(*right_element, left_key);
                }

                self.items
                    .insert(left_key, left_potential.merge(right_potential));
            }
        };
    }

    fn supplied_by(&mut self, identifier: ElectricalElementIdentifier, potential: NewPotential) {
        let key = self.element_to_potential_key.get_or_insert(identifier);
        self.items.insert(key, potential);
    }

    fn is_powered(&self, identifier: ElectricalElementIdentifier) -> bool {
        if let Some(key) = self.element_to_potential_key.get(identifier) {
            match self.items.get(&key) {
                Some(potential) => potential.is_powered(),
                None => false,
            }
        } else {
            false
        }
    }

    fn get(&self, identifier: ElectricalElementIdentifier) -> Option<&NewPotential> {
        if let Some(key) = self.element_to_potential_key.get(identifier) {
            self.items.get(&key)
        } else {
            None
        }
    }

    fn consume_from(&mut self, identifier: ElectricalElementIdentifier, power: Power) {
        if let Some(key) = self.element_to_potential_key.get(identifier) {
            match self.items.get_mut(&key) {
                Some(potential) => {
                    for origin in potential.origins() {
                        let y = self.consumption_per_origin.entry(*origin).or_default();
                        *y += power / potential.origin_count() as f64;
                    }
                }
                None => {}
            }
        }
    }

    fn total_consumption_of(&self, origin: PotentialOrigin) -> Power {
        match self.consumption_per_origin.get(&origin) {
            Some(power) => *power,
            None => Power::new::<watt>(0.),
        }
    }
}

#[cfg(test)]
mod new_potential_tests {
    use super::*;

    #[test]
    fn some_potential_is_powered() {
        assert_eq!(some_potential().is_powered(), true);
    }

    #[test]
    fn some_potential_is_not_unpowered() {
        assert_eq!(some_potential().is_unpowered(), false);
    }

    #[test]
    fn none_potential_is_not_powered() {
        assert_eq!(none_potential().is_powered(), false);
    }

    #[test]
    fn none_potential_is_unpowered() {
        assert_eq!(none_potential().is_unpowered(), true);
    }

    #[test]
    fn merge_ignores_none() {
        let potential = NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        )
        .merge(NewPotential::none());

        assert!(potential.is_only_powered_by_apu());
        assert_eq!(potential.raw, ElectricPotential::new::<volt>(115.));
    }

    #[test]
    fn merge_returns_the_callee_when_its_potential_is_greater_than_that_of_the_argument() {
        let potential = NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        )
        .merge(NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(40.),
        ));

        assert!(potential.is_only_powered_by_apu());
        assert_eq!(potential.raw, ElectricPotential::new::<volt>(115.));
    }

    #[test]
    fn merge_returns_the_argument_when_its_potential_is_greater_than_that_of_the_callee() {
        let potential = NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(40.),
        )
        .merge(NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        ));

        assert!(potential.is_only_powered_by_apu());
        assert_eq!(potential.raw, ElectricPotential::new::<volt>(115.));
    }

    #[test]
    fn merge_returns_a_merged_result_when_both_callee_and_argument_have_equal_potential() {
        let potential = NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        )
        .merge(NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(115.),
        ));

        let mut set = HashSet::new();
        set.insert(PotentialOrigin::ApuGenerator(1));
        set.insert(PotentialOrigin::EngineGenerator(1));

        assert_eq!(potential.origins.symmetric_difference(&set).count(), 0);
        assert_eq!(potential.raw, ElectricPotential::new::<volt>(115.));
    }

    #[test]
    fn merge_combines_equal_origins() {
        let potential = NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        )
        .merge(NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        ));

        assert!(potential.is_only_powered_by_apu());
        assert_eq!(potential.raw, ElectricPotential::new::<volt>(115.));
    }

    #[test]
    fn merge_considers_miniscule_potential_differences_equal() {
        let potential = NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(115.0011),
        )
        .merge(NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.002),
        ));

        let mut set = HashSet::new();
        set.insert(PotentialOrigin::ApuGenerator(1));
        set.insert(PotentialOrigin::EngineGenerator(1));

        assert_eq!(potential.origins.symmetric_difference(&set).count(), 0);
    }

    #[test]
    fn merge_considers_larger_potential_differences_inequal() {
        let potential = NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(115.001),
        )
        .merge(NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.0021),
        ));

        assert_eq!(potential.origins.len(), 1);
        assert_eq!(
            potential.origins.iter().next(),
            Some(&PotentialOrigin::ApuGenerator(1))
        );
    }

    #[test]
    fn merge_takes_the_lowest_raw_potential_from_two_potentials_it_considers_equal() {
        let potential = NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(115.0011),
        )
        .merge(NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.002),
        ));

        assert_eq!(potential.raw, ElectricPotential::new::<volt>(115.0011));
    }

    #[test]
    fn origin_count_returns_0_when_none() {
        assert_eq!(NewPotential::none().origin_count(), 0);
    }

    #[test]
    fn origin_count_returns_the_number_of_origins() {
        let potential = NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        )
        .merge(NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(115.),
        ));

        assert_eq!(potential.origin_count(), 2);
    }

    #[test]
    fn is_only_powered_by_single_engine_generator_returns_false_when_none() {
        assert!(!NewPotential::none().is_only_powered_by_single_engine_generator());
    }

    #[test]
    fn is_only_powered_by_single_engine_generator_returns_false_when_different_origin() {
        assert!(!NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.)
        )
        .is_only_powered_by_single_engine_generator());
    }

    #[test]
    fn is_only_powered_by_single_engine_generator_returns_true_when_engine_generator() {
        assert!(NewPotential::new(
            PotentialOrigin::EngineGenerator(2),
            ElectricPotential::new::<volt>(115.)
        )
        .is_only_powered_by_single_engine_generator());
    }

    #[test]
    fn is_only_powered_by_single_engine_generator_returns_false_when_pair_of_engine_generators() {
        assert!(!NewPotential::new(
            PotentialOrigin::EngineGenerator(1),
            ElectricPotential::new::<volt>(115.)
        )
        .merge(NewPotential::new(
            PotentialOrigin::EngineGenerator(2),
            ElectricPotential::new::<volt>(115.)
        ))
        .is_only_powered_by_single_engine_generator());
    }

    fn some_potential() -> NewPotential {
        NewPotential::new(
            PotentialOrigin::ApuGenerator(1),
            ElectricPotential::new::<volt>(115.),
        )
    }

    fn none_potential() -> NewPotential {
        NewPotential::none()
    }
}

#[cfg(test)]
mod electricity_tests {
    use super::*;

    struct TestElectricalElement {
        number: usize,
        identifier: ElectricalElementIdentifier,
        is_powered: bool,
        is_conductive: bool,
    }
    impl TestElectricalElement {
        fn new(identifier_provider: &mut impl ElectricalElementIdentifierProvider) -> Self {
            Self {
                number: 1,
                identifier: identifier_provider.next(),
                is_powered: false,
                is_conductive: true,
            }
        }

        fn unregistered() -> Self {
            Self {
                number: 1,
                identifier: ElectricalElementIdentifier::first(),
                is_powered: false,
                is_conductive: true,
            }
        }

        fn power(mut self) -> Self {
            self.is_powered = true;
            self
        }

        fn non_conductive(mut self) -> Self {
            self.is_conductive = false;
            self
        }

        fn with_number(mut self, number: usize) -> Self {
            self.number = number;
            self
        }
    }
    impl ElectricalElement for TestElectricalElement {
        fn input_identifier(&self) -> ElectricalElementIdentifier {
            self.identifier
        }

        fn output_identifier(&self) -> ElectricalElementIdentifier {
            self.identifier
        }

        fn is_conductive(&self) -> bool {
            self.is_conductive
        }
    }
    impl ElectricitySource for TestElectricalElement {
        fn output_potential(&self) -> NewPotential {
            if self.is_powered {
                NewPotential::new(
                    PotentialOrigin::EngineGenerator(self.number),
                    ElectricPotential::new::<volt>(115.),
                )
            } else {
                NewPotential::none()
            }
        }
    }

    struct TestBus {
        identifier: ElectricalElementIdentifier,
    }
    impl TestBus {
        fn new(
            bus_type: ElectricalBusType,
            identifier_provider: &mut impl ElectricalElementIdentifierProvider,
        ) -> Self {
            Self {
                identifier: identifier_provider.next_for_bus(bus_type),
            }
        }
    }
    impl ElectricalElement for TestBus {
        fn input_identifier(&self) -> ElectricalElementIdentifier {
            self.identifier
        }

        fn output_identifier(&self) -> ElectricalElementIdentifier {
            self.identifier
        }

        fn is_conductive(&self) -> bool {
            true
        }
    }

    struct TestTransformer {
        input_identifier: ElectricalElementIdentifier,
        output_identifier: ElectricalElementIdentifier,
    }
    impl TestTransformer {
        fn new(identifier_provider: &mut impl ElectricalElementIdentifierProvider) -> Self {
            Self {
                input_identifier: identifier_provider.next(),
                output_identifier: identifier_provider.next(),
            }
        }
    }
    impl ElectricityTransformer for TestTransformer {
        fn transform(&self, input: &NewPotential) -> NewPotential {
            if input.is_powered() {
                NewPotential::new(
                    PotentialOrigin::TransformerRectifier(1),
                    ElectricPotential::new::<volt>(28.),
                )
            } else {
                NewPotential::none()
            }
        }
    }
    impl ElectricalElement for TestTransformer {
        fn input_identifier(&self) -> ElectricalElementIdentifier {
            self.input_identifier
        }

        fn output_identifier(&self) -> ElectricalElementIdentifier {
            self.output_identifier
        }

        fn is_conductive(&self) -> bool {
            true
        }
    }

    #[test]
    fn next_provides_increasing_identifiers() {
        let mut electricity = Electricity::new();
        let mut identifier = electricity.next();
        for _ in 1..=10 {
            let next_identifier = electricity.next();
            assert!(identifier.0 < next_identifier.0);
            identifier = next_identifier;
        }
    }

    #[test]
    fn next_for_bus_provides_increasing_identifiers() {
        let mut electricity = Electricity::new();
        let mut identifier = electricity.next();
        for _ in 1..=10 {
            let next_identifier = electricity.next_for_bus(ElectricalBusType::DirectCurrentBattery);
            assert!(identifier.0 < next_identifier.0);
            identifier = next_identifier;
        }
    }

    #[test]
    fn next_and_next_for_bus_together_provide_increasing_identifiers() {
        let mut electricity = Electricity::new();
        let mut identifier = electricity.next();
        for n in 1..=10 {
            let next_identifier = if n % 2 == 0 {
                electricity.next()
            } else {
                electricity.next_for_bus(ElectricalBusType::DirectCurrentBattery)
            };
            assert!(identifier.0 < next_identifier.0);
            identifier = next_identifier;
        }
    }

    #[test]
    fn next_for_bus_identifier_matches_looked_up_identifier() {
        let mut electricity = Electricity::new();

        electricity.next_for_bus(ElectricalBusType::DirectCurrentBattery);
        let identifier = electricity.next_for_bus(ElectricalBusType::DirectCurrentEssential);

        assert!(
            electricity.identifier_for(ElectricalBusType::DirectCurrentEssential)
                == Some(&identifier)
        );
    }

    #[test]
    fn an_unknown_element_isnt_powered() {
        let electricity = Electricity::new();
        let element = TestElectricalElement::unregistered();

        assert!(!electricity.is_powered(&element));
    }

    #[test]
    fn a_known_but_unpowered_element_isnt_powered() {
        let mut electricity = Electricity::new();
        let element = TestElectricalElement::new(&mut electricity);

        assert!(!electricity.is_powered(&element));
    }

    #[test]
    fn a_known_and_powered_element_is_powered() {
        let mut electricity = Electricity::new();
        let element = TestElectricalElement::new(&mut electricity).power();
        electricity.supplied_by(&element);

        assert!(electricity.is_powered(&element));
    }

    #[test]
    fn power_doesnt_flow_from_an_unpowered_element_to_another_unpowered_element() {
        let mut electricity = Electricity::new();
        let from_element = TestElectricalElement::new(&mut electricity);
        let to_element = TestElectricalElement::new(&mut electricity);
        electricity.supplied_by(&from_element);
        electricity.flow(&from_element, &to_element);

        assert!(!electricity.is_powered(&to_element));
    }

    #[test]
    fn power_flows_from_a_powered_to_an_unpowered_element() {
        let mut electricity = Electricity::new();
        let from_element = TestElectricalElement::new(&mut electricity).power();
        let to_element = TestElectricalElement::new(&mut electricity);
        electricity.supplied_by(&from_element);
        electricity.flow(&from_element, &to_element);

        assert!(electricity.is_powered(&to_element));
    }

    #[test]
    fn power_doesnt_flow_from_a_non_conductive_to_a_conductive_element() {
        let mut electricity = Electricity::new();
        let from_element = TestElectricalElement::new(&mut electricity)
            .power()
            .non_conductive();
        let to_element = TestElectricalElement::new(&mut electricity);
        electricity.supplied_by(&from_element);
        electricity.flow(&from_element, &to_element);

        assert!(!electricity.is_powered(&to_element));
    }

    #[test]
    fn power_doesnt_flow_from_a_conductive_to_a_non_conductive_element() {
        let mut electricity = Electricity::new();
        let from_element = TestElectricalElement::new(&mut electricity).power();
        let to_element = TestElectricalElement::new(&mut electricity).non_conductive();
        electricity.supplied_by(&from_element);
        electricity.flow(&from_element, &to_element);

        assert!(!electricity.is_powered(&to_element));
    }

    #[test]
    fn an_unknown_bus_isnt_powered() {
        let electricity = Electricity::new();

        assert!(!electricity.bus_is_powered(ElectricalBusType::DirectCurrentBattery));
    }

    #[test]
    fn a_known_but_unpowered_bus_isnt_powered() {
        let mut electricity = Electricity::new();
        // Request an identifier, such that the bus type is registered.
        TestBus::new(ElectricalBusType::DirectCurrentBattery, &mut electricity);

        assert!(!electricity.bus_is_powered(ElectricalBusType::DirectCurrentBattery));
    }

    #[test]
    fn a_known_and_powered_bus_is_powered() {
        let mut electricity = Electricity::new();
        let element = TestElectricalElement::new(&mut electricity).power();
        electricity.supplied_by(&element);

        let bus = TestBus::new(ElectricalBusType::DirectCurrentBattery, &mut electricity);
        electricity.flow(&element, &bus);

        assert!(electricity.bus_is_powered(ElectricalBusType::DirectCurrentBattery));
    }

    #[test]
    fn an_unpowered_transformer_does_not_transform_electricity() {
        let mut electricity = Electricity::new();
        let transformer = TestTransformer::new(&mut electricity);
        electricity.transform_in(&transformer);

        assert!(!electricity.is_powered(&transformer));
    }

    #[test]
    fn a_powered_transformer_transforms_electricity() {
        let mut electricity = Electricity::new();
        let source = TestElectricalElement::new(&mut electricity).power();
        let transformer = TestTransformer::new(&mut electricity);

        electricity.supplied_by(&source);
        electricity.flow(&source, &transformer);
        electricity.transform_in(&transformer);

        assert!(electricity.is_powered(&transformer));
    }

    #[test]
    fn transformed_electricity_isnt_the_same_potential() {
        let mut electricity = Electricity::new();
        let source = TestElectricalElement::new(&mut electricity).power();
        let transformer = TestTransformer::new(&mut electricity);

        electricity.supplied_by(&source);
        electricity.flow(&source, &transformer);
        electricity.transform_in(&transformer);

        assert!(electricity
            .output_of(&source)
            .origins
            .contains(&PotentialOrigin::EngineGenerator(1)));
        assert!(electricity
            .output_of(&transformer)
            .origins
            .contains(&PotentialOrigin::TransformerRectifier(1)))
    }

    #[test]
    fn electricity_flows_back_through_the_circuit() {
        let mut electricity = Electricity::new();
        let first = TestElectricalElement::new(&mut electricity);
        let second = TestElectricalElement::new(&mut electricity);
        let third = TestElectricalElement::new(&mut electricity).power();

        electricity.flow(&first, &second);
        electricity.flow(&second, &third);
        electricity.supplied_by(&third);

        assert!(electricity.is_powered(&first));
    }

    #[test]
    fn electricity_from_multiple_sources_combines() {
        let mut electricity = Electricity::new();
        let first = TestElectricalElement::new(&mut electricity).power();
        let second = TestElectricalElement::new(&mut electricity)
            .with_number(2)
            .power();

        electricity.supplied_by(&first);
        electricity.supplied_by(&second);
        electricity.flow(&first, &second);

        assert_eq!(electricity.output_of(&first).origin_count(), 2);
    }

    #[test]
    fn flow_right_argument_doesnt_dangle() {
        let mut electricity = Electricity::new();
        let first = TestElectricalElement::new(&mut electricity);
        let second = TestElectricalElement::new(&mut electricity);
        let third = TestElectricalElement::new(&mut electricity);
        let fourth = TestElectricalElement::new(&mut electricity);
        electricity.flow(&first, &second);

        // If the code doesn't add the third element to the potential object during this call...
        electricity.flow(&second, &third);

        let powered = TestElectricalElement::new(&mut electricity).power();
        electricity.supplied_by(&powered);
        electricity.flow(&fourth, &powered);

        // ...then during this call the third element would get lost, as the potential doesn't contain
        // a reference to the element, and thus there is no way to include that element in the new merged potential.
        electricity.flow(&fourth, &first);

        assert!(electricity.is_powered(&third));
    }

    #[test]
    fn flow_left_argument_doesnt_dangle() {
        let mut electricity = Electricity::new();
        let first = TestElectricalElement::new(&mut electricity);
        let second = TestElectricalElement::new(&mut electricity);
        let third = TestElectricalElement::new(&mut electricity);
        let fourth = TestElectricalElement::new(&mut electricity);
        electricity.flow(&first, &second);

        // If the code doesn't add the third element to the potential object during this call...
        electricity.flow(&third, &second);

        let powered = TestElectricalElement::new(&mut electricity).power();
        electricity.supplied_by(&powered);
        electricity.flow(&fourth, &powered);

        // ...then during this call the third element would get lost, as the potential doesn't contain
        // a reference to the element, and thus there is no way to include that element in the new merged potential.
        electricity.flow(&fourth, &first);

        assert!(electricity.is_powered(&third));
    }

    #[test]
    fn any_is_powered_returns_true_when_any_of_the_arguments_is_powered() {
        let mut electricity = Electricity::new();
        let element = TestElectricalElement::new(&mut electricity).power();
        electricity.supplied_by(&element);

        let ac_bus = TestBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);
        TestBus::new(ElectricalBusType::DirectCurrent(1), &mut electricity);

        electricity.flow(&element, &ac_bus);
        // Don't flow to DC BUS on purpose.

        assert!(electricity.any_is_powered(&[
            ElectricalBusType::AlternatingCurrent(1),
            ElectricalBusType::DirectCurrent(1),
        ]));
    }

    #[test]
    fn any_is_powered_returns_false_when_none_of_the_arguments_is_powered() {
        let mut electricity = Electricity::new();
        TestBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);
        TestBus::new(ElectricalBusType::DirectCurrent(1), &mut electricity);

        assert!(!electricity.any_is_powered(&[
            ElectricalBusType::AlternatingCurrent(1),
            ElectricalBusType::DirectCurrent(1),
        ]));
    }

    #[test]
    fn potential_of_returns_a_potential_which_isnt_powered_when_bus_is_unpowered() {
        let mut electricity = Electricity::new();
        TestBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);
        let potential = electricity.potential_of_bus(ElectricalBusType::AlternatingCurrent(1));

        assert!(potential.is_unpowered());
    }

    #[test]
    fn potential_of_returns_a_potential_with_origin_when_bus_is_powered() {
        let mut electricity = Electricity::new();
        let element = TestElectricalElement::new(&mut electricity).power();
        electricity.supplied_by(&element);

        let bus = TestBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);
        electricity.flow(&element, &bus);

        let potential = electricity.potential_of_bus(ElectricalBusType::AlternatingCurrent(1));

        assert!(potential.is_only_powered_by_single_engine_generator());
    }

    #[test]
    fn power_consumed_from_a_powered_bus_is_included_in_the_power_usage() {
        let mut electricity = Electricity::new();
        let generator = TestElectricalElement::new(&mut electricity).power();
        electricity.supplied_by(&generator);

        let bus = TestBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);
        electricity.flow(&generator, &bus);

        electricity.consume_from_bus(
            ElectricalBusType::AlternatingCurrent(1),
            Power::new::<watt>(400.),
        );
        electricity.consume_from_bus(
            ElectricalBusType::AlternatingCurrent(1),
            Power::new::<watt>(300.),
        );

        assert_eq!(
            electricity.total_consumption_of(PotentialOrigin::EngineGenerator(1)),
            Power::new::<watt>(700.)
        );
    }

    #[test]
    fn power_consumed_from_an_unpowered_bus_is_not_included_in_the_power_usage() {
        let mut electricity = Electricity::new();
        TestBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);

        electricity.consume_from_bus(
            ElectricalBusType::AlternatingCurrent(1),
            Power::new::<watt>(400.),
        );

        assert_eq!(
            electricity.total_consumption_of(PotentialOrigin::EngineGenerator(1)),
            Power::new::<watt>(0.)
        );
    }

    #[test]
    fn consumption_is_equally_divided_between_all_potential_origins() {
        let mut electricity = Electricity::new();
        let generator_1 = TestElectricalElement::new(&mut electricity).power();
        let generator_2 = TestElectricalElement::new(&mut electricity)
            .with_number(2)
            .power();
        electricity.supplied_by(&generator_1);
        electricity.supplied_by(&generator_2);

        let bus = TestBus::new(ElectricalBusType::AlternatingCurrent(1), &mut electricity);
        electricity.flow(&generator_1, &bus);
        electricity.flow(&generator_2, &bus);

        electricity.consume_from_bus(
            ElectricalBusType::AlternatingCurrent(1),
            Power::new::<watt>(400.),
        );

        assert_eq!(
            electricity.total_consumption_of(PotentialOrigin::EngineGenerator(1)),
            Power::new::<watt>(200.)
        );
        assert_eq!(
            electricity.total_consumption_of(PotentialOrigin::EngineGenerator(2)),
            Power::new::<watt>(200.)
        );
    }
}
