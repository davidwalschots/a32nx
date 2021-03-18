class A32NX_Core {
    constructor() {
        this.modules = [
            new A32NX_ADIRS(),
            new A32NX_APU(),
            new A32NX_BaroSelector(),
            new A32NX_BrakeTemp(),
            new A32NX_Electricity(),
            new A32NX_LocalVarUpdater(),
            new A32NX_FADEC(1),
            new A32NX_FADEC(2),
            new A32NX_FWC(),
            new A32NX_GPWS(this),
            new A32NX_GroundReference(),
            new A32NX_Speeds()
        ];

        this.soundManager = new A32NX_SoundManager();
    }

    init(startTime) {
        this.getDeltaTime = A32NX_Util.createDeltaTimeCalculator(startTime);
        this.modules.forEach(module => {
            if (typeof module.init === "function") {
                module.init();
            }
        });
        this.isInit = true;
    }

    update() {
        if (!this.isInit) {
            return;
        }

        const deltaTime = this.getDeltaTime();

        this.soundManager.update(deltaTime);
        this.modules.forEach(module => {
            module.update(deltaTime, this);
        });
    }
}
