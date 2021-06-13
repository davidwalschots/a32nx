use super::{
    ElectricalElement, ElectricalElementIdentifier, ElectricalElementIdentifierProvider,
    ElectricalStateWriter, ElectricityTransformer, NewPotential, PotentialOrigin, ProvideCurrent,
    ProvidePotential,
};
use crate::{
    shared::{ConsumePower, PowerConsumptionReport},
    simulation::{SimulationElement, SimulatorWriter},
};
use uom::si::{electric_current::ampere, electric_potential::volt, f64::*};

pub struct TransformerRectifier {
    writer: ElectricalStateWriter,
    number: usize,
    input_identifier: ElectricalElementIdentifier,
    output_identifier: ElectricalElementIdentifier,
    failed: bool,
    output_potential: ElectricPotential,
    output_current: ElectricCurrent,
}
impl TransformerRectifier {
    pub fn new(
        number: usize,
        identifier_provider: &mut impl ElectricalElementIdentifierProvider,
    ) -> TransformerRectifier {
        TransformerRectifier {
            writer: ElectricalStateWriter::new(&format!("TR_{}", number)),
            number,
            input_identifier: identifier_provider.next(),
            output_identifier: identifier_provider.next(),
            failed: false,
            output_potential: ElectricPotential::new::<volt>(0.),
            output_current: ElectricCurrent::new::<ampere>(0.),
        }
    }

    pub fn fail(&mut self) {
        self.failed = true;
    }

    pub fn failed(&self) -> bool {
        self.failed
    }
}
impl ProvideCurrent for TransformerRectifier {
    fn current(&self) -> ElectricCurrent {
        self.output_current
    }

    fn current_normal(&self) -> bool {
        self.output_current > ElectricCurrent::new::<ampere>(5.)
    }
}
provide_potential!(TransformerRectifier, (25.0..=31.0));
impl ElectricalElement for TransformerRectifier {
    fn input_identifier(&self) -> ElectricalElementIdentifier {
        self.input_identifier
    }

    fn output_identifier(&self) -> ElectricalElementIdentifier {
        self.output_identifier
    }

    fn is_conductive(&self) -> bool {
        !self.failed
    }
}
impl ElectricityTransformer for TransformerRectifier {
    fn transform(&self, input: &NewPotential) -> super::NewPotential {
        if !self.failed && input.is_powered() {
            NewPotential::new(
                PotentialOrigin::TransformerRectifier(self.number),
                ElectricPotential::new::<volt>(28.),
            )
        } else {
            NewPotential::none()
        }
    }
}
impl SimulationElement for TransformerRectifier {
    fn write(&self, writer: &mut SimulatorWriter) {
        self.writer.write_direct(self, writer);
    }

    fn consume_power_in_converters<T: ConsumePower>(&mut self, consumption: &mut T) {
        let dc_power =
            consumption.total_consumption_of(PotentialOrigin::TransformerRectifier(self.number));

        // Add the DC consumption to the TRs input (AC) consumption.
        // Currently transformer rectifier inefficiency isn't modelled.
        // It is to be expected that AC consumption should actually be somewhat
        // higher than DC consumption.
        consumption.consume_from_input(self, dc_power);
    }

    fn process_power_consumption_report<T: PowerConsumptionReport>(&mut self, report: &T) {
        self.output_potential = if report.is_powered(self) {
            ElectricPotential::new::<volt>(28.)
        } else {
            ElectricPotential::new::<volt>(0.)
        };

        let consumption =
            report.total_consumption_of(PotentialOrigin::TransformerRectifier(self.number));
        self.output_current = consumption / self.output_potential;
    }
}

#[cfg(test)]
mod transformer_rectifier_tests {
    use uom::si::power::watt;

    use super::*;
    use crate::{
        electrical::{
            consumption::PowerConsumer, test::TestElectricitySource, ElectricalBus,
            ElectricalBusType, Electricity, PotentialOrigin,
        },
        simulation::{test::SimulationTestBed, Aircraft, SimulationElementVisitor, UpdateContext},
    };

    struct TransformerRectifierTestBed {
        test_bed: SimulationTestBed,
    }
    impl TransformerRectifierTestBed {
        fn new() -> Self {
            Self {
                test_bed: SimulationTestBed::new(),
            }
        }

        fn run_aircraft(&mut self, aircraft: &mut impl Aircraft) {
            self.test_bed.run_aircraft(aircraft);
        }

        fn current_is_normal(&mut self) -> bool {
            self.test_bed.read_bool("ELEC_TR_1_CURRENT_NORMAL")
        }

        fn potential_is_normal(&mut self) -> bool {
            self.test_bed.read_bool("ELEC_TR_1_POTENTIAL_NORMAL")
        }

        fn current(&mut self) -> ElectricCurrent {
            ElectricCurrent::new::<ampere>(self.test_bed.read_f64("ELEC_TR_1_CURRENT"))
        }

        fn electricity_mut(&mut self) -> &mut Electricity {
            self.test_bed.electricity_mut()
        }
    }

    struct TestAircraft {
        electricity_source: TestElectricitySource,
        transformer_rectifier: TransformerRectifier,
        bus: ElectricalBus,
        consumer: PowerConsumer,
        transformer_rectifier_consumption: Power,
    }
    impl TestAircraft {
        fn new(electricity: &mut Electricity) -> Self {
            Self {
                electricity_source: TestElectricitySource::unpowered(
                    PotentialOrigin::ApuGenerator(1),
                    electricity,
                ),
                transformer_rectifier: TransformerRectifier::new(1, electricity),
                bus: ElectricalBus::new(ElectricalBusType::DirectCurrent(1), electricity),
                consumer: PowerConsumer::from(ElectricalBusType::DirectCurrent(1)),
                transformer_rectifier_consumption: Power::new::<watt>(0.),
            }
        }

        fn with_powered_transformer_rectifier(mut self) -> Self {
            self.electricity_source.power();
            self
        }

        fn with_unpowered_transformer_rectifier(mut self) -> Self {
            self.electricity_source.unpower();
            self
        }

        fn fail_transformer_rectifier(&mut self) {
            self.transformer_rectifier.fail();
        }

        fn transformer_rectifier_is_powered(&self, electricity: &Electricity) -> bool {
            electricity.is_powered(&self.transformer_rectifier)
        }

        fn power_demand(&mut self, power: Power) {
            self.consumer.demand(power);
        }

        fn transformer_rectifier_consumption(&self) -> Power {
            self.transformer_rectifier_consumption
        }
    }
    impl Aircraft for TestAircraft {
        fn update_before_power_distribution(
            &mut self,
            _: &UpdateContext,
            electricity: &mut Electricity,
        ) {
            electricity.supplied_by(&self.electricity_source);
            electricity.flow(&self.electricity_source, &self.transformer_rectifier);
            electricity.transform_in(&self.transformer_rectifier);
            electricity.flow(&self.transformer_rectifier, &self.bus);
        }
    }
    impl SimulationElement for TestAircraft {
        fn accept<T: SimulationElementVisitor>(&mut self, visitor: &mut T) {
            self.transformer_rectifier.accept(visitor);
            self.consumer.accept(visitor);

            visitor.visit(self);
        }

        fn process_power_consumption_report<T: PowerConsumptionReport>(&mut self, report: &T) {
            self.transformer_rectifier_consumption =
                report.total_consumption_of(PotentialOrigin::TransformerRectifier(1));
        }
    }

    #[test]
    fn when_unpowered_has_no_output() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_unpowered_transformer_rectifier();

        test_bed.run_aircraft(&mut aircraft);

        assert!(!aircraft.transformer_rectifier_is_powered(test_bed.electricity_mut()));
    }

    #[test]
    fn when_powered_has_output() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        test_bed.run_aircraft(&mut aircraft);

        assert!(aircraft.transformer_rectifier_is_powered(test_bed.electricity_mut()));
    }

    #[test]
    fn when_powered_but_failed_has_no_output() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        aircraft.fail_transformer_rectifier();
        test_bed.run_aircraft(&mut aircraft);

        assert!(!aircraft.transformer_rectifier_is_powered(test_bed.electricity_mut()));
    }

    #[test]
    fn when_unpowered_current_is_not_normal() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_unpowered_transformer_rectifier();

        test_bed.run_aircraft(&mut aircraft);

        assert!(!test_bed.current_is_normal());
    }

    #[test]
    fn when_powered_with_too_little_demand_current_is_not_normal() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        aircraft.power_demand(Power::new::<watt>(5. * 28.));
        test_bed.run_aircraft(&mut aircraft);

        assert!(!test_bed.current_is_normal());
    }

    #[test]
    fn when_powered_with_enough_demand_current_is_normal() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        aircraft.power_demand(Power::new::<watt>((5. * 28.) + 1.));
        test_bed.run_aircraft(&mut aircraft);

        assert!(test_bed.current_is_normal());
    }

    #[test]
    fn when_unpowered_potential_is_not_normal() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_unpowered_transformer_rectifier();

        test_bed.run_aircraft(&mut aircraft);

        assert!(!test_bed.potential_is_normal());
    }

    #[test]
    fn when_powered_potential_is_normal() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        test_bed.run_aircraft(&mut aircraft);

        assert!(test_bed.potential_is_normal());
    }

    #[test]
    fn when_unpowered_has_no_consumption() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_unpowered_transformer_rectifier();

        test_bed.run_aircraft(&mut aircraft);

        assert_eq!(
            aircraft.transformer_rectifier_consumption(),
            Power::new::<watt>(0.)
        );
    }

    #[test]
    fn when_powered_without_demand_has_no_consumption() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        aircraft.power_demand(Power::new::<watt>(0.));
        test_bed.run_aircraft(&mut aircraft);

        assert_eq!(
            aircraft.transformer_rectifier_consumption(),
            Power::new::<watt>(0.)
        );
    }

    #[test]
    fn when_powered_with_demand_has_consumption() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        aircraft.power_demand(Power::new::<watt>(200.));
        test_bed.run_aircraft(&mut aircraft);

        assert_eq!(
            aircraft.transformer_rectifier_consumption(),
            Power::new::<watt>(200.)
        );
    }

    #[test]
    fn when_powered_with_demand_current_is_based_on_demand() {
        let mut test_bed = TransformerRectifierTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        aircraft.power_demand(Power::new::<watt>(200.));
        test_bed.run_aircraft(&mut aircraft);

        assert_eq!(
            test_bed.current(),
            ElectricCurrent::new::<ampere>(200. / 28.)
        );
    }

    #[test]
    fn writes_its_state() {
        let mut test_bed = SimulationTestBed::new();
        let mut aircraft =
            TestAircraft::new(test_bed.electricity_mut()).with_powered_transformer_rectifier();

        test_bed.run_aircraft(&mut aircraft);

        assert!(test_bed.contains_key("ELEC_TR_1_CURRENT"));
        assert!(test_bed.contains_key("ELEC_TR_1_CURRENT_NORMAL"));
        assert!(test_bed.contains_key("ELEC_TR_1_POTENTIAL"));
        assert!(test_bed.contains_key("ELEC_TR_1_POTENTIAL_NORMAL"));
    }
}
