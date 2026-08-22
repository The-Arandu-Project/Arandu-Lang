export interface CrashDecision {
    readonly restart: boolean;
    readonly crashCount: number;
}

const DEFAULT_MAX_RESTARTS = 3;
const DEFAULT_WINDOW_MS = 3 * 60 * 1000;

export class CrashRestartPolicy {
    private readonly crashes: number[] = [];

    public constructor(
        private readonly maxRestarts = DEFAULT_MAX_RESTARTS,
        private readonly windowMs = DEFAULT_WINDOW_MS
    ) {}

    public recordCrash(now: number): CrashDecision {
        const windowStart = now - this.windowMs;
        while (this.crashes.length > 0 && this.crashes[0] < windowStart) {
            this.crashes.shift();
        }
        this.crashes.push(now);
        return {
            restart: this.crashes.length <= this.maxRestarts,
            crashCount: this.crashes.length
        };
    }
}
