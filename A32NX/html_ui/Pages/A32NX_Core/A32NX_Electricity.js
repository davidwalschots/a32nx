class A32NX_Electricity {
    constructor() {
        console.log('A32NX_Electricity constructed');
    }

    init() {
        console.log('A32NX_Electricity init');
    }

    update() {
        this.toggleMasterBattery(1);
    }

    toggleMasterBattery(number) {
        if (!SimVar.GetSimVarValue("A:ELECTRICAL MASTER BATTERY:" + number, "Bool")) {
            SimVar.SetSimVarValue("K:TOGGLE_MASTER_BATTERY", "Number", number);
        }
    }
}
