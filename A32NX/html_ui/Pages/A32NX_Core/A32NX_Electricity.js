class A32NX_Electricity {
    constructor() {
        console.log('A32NX_Electricity constructed');
    }
    init() {
        console.log('A32NX_Electricity init');
    }
    update(_deltaTime, _core) {
        this.toggleMasterBattery(1);

        if (_core.ACPowerStateChange) {
            const ACPowerAvailable = SimVar.GetSimVarValue("L:ACPowerAvailable","Bool");
            const screenBlueLightsCircuitOn = SimVar.GetSimVarValue("A:CIRCUIT ON:79","Bool");
            if (ACPowerAvailable) {
                if (!screenBlueLightsCircuitOn) {
                    SimVar.SetSimVarValue("K:ELECTRICAL_CIRCUIT_TOGGLE", "number", 79);
                }
            } else {
                if (screenBlueLightsCircuitOn) {
                    SimVar.SetSimVarValue("K:ELECTRICAL_CIRCUIT_TOGGLE", "number", 79);
                }
            }
        }
    }

    toggleMasterBattery(number) {
        if (!SimVar.GetSimVarValue("A:ELECTRICAL MASTER BATTERY:" + number, "Bool")) {
            SimVar.SetSimVarValue("K:TOGGLE_MASTER_BATTERY", "Number", number);
        }
    }
}
