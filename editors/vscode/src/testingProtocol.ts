export const TEST_PROTOCOL_V1 = 'arandu.test/v1';
export const TEST_LIST_PROTOCOL_V1 = 'arandu.test-list/v1';
export const BENCH_LIST_PROTOCOL_V1 = 'arandu.bench-list/v1';

export interface TestJsonReport {
    readonly schema: typeof TEST_PROTOCOL_V1;
    readonly cases: readonly TestJsonCase[];
}

export interface TestJsonCase {
    readonly id: string;
    readonly status: 'passed' | 'failed' | 'skipped' | 'timed_out' | 'crashed';
    readonly duration_ms: number;
    readonly stdout: string;
    readonly stderr: string;
    readonly failure?: { readonly message?: string; readonly location?: string } | null;
}

export interface DiscoveryReport {
    readonly schema: string;
    readonly cases: readonly DiscoveryJsonCase[];
}

export interface DiscoveryJsonCase {
    readonly id: string;
    readonly path: string;
    readonly line: number;
    readonly column_utf16: number;
}

const TEST_STATUSES = new Set(['passed', 'failed', 'skipped', 'timed_out', 'crashed']);

export function parseTestReport(stdout: string): TestJsonReport | undefined {
    const parsed = parseObject(stdout);
    if (!parsed || parsed.schema !== TEST_PROTOCOL_V1 || !Array.isArray(parsed.cases)) {
        return undefined;
    }
    if (!parsed.cases.every(isTestCase)) {
        return undefined;
    }
    return parsed as unknown as TestJsonReport;
}

export function parseDiscoveryReport(stdout: string, schema: string): DiscoveryReport | undefined {
    const parsed = parseObject(stdout);
    if (!parsed || parsed.schema !== schema || !Array.isArray(parsed.cases)) {
        return undefined;
    }
    if (!parsed.cases.every(isDiscoveryCase)) {
        return undefined;
    }
    return parsed as unknown as DiscoveryReport;
}

function parseObject(text: string): Record<string, unknown> | undefined {
    try {
        const value: unknown = JSON.parse(text);
        return isObject(value) ? value : undefined;
    } catch {
        return undefined;
    }
}

function isTestCase(value: unknown): boolean {
    if (!isObject(value)) {
        return false;
    }
    const failure = value.failure;
    return typeof value.id === 'string'
        && typeof value.status === 'string'
        && TEST_STATUSES.has(value.status)
        && finiteNonNegative(value.duration_ms)
        && typeof value.stdout === 'string'
        && typeof value.stderr === 'string'
        && (failure === undefined || failure === null || (
            isObject(failure)
            && (failure.message === undefined || typeof failure.message === 'string')
            && (failure.location === undefined || typeof failure.location === 'string')
        ));
}

function isDiscoveryCase(value: unknown): boolean {
    return isObject(value)
        && typeof value.id === 'string'
        && typeof value.path === 'string'
        && integerNonNegative(value.line)
        && integerNonNegative(value.column_utf16);
}

function isObject(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function finiteNonNegative(value: unknown): value is number {
    return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function integerNonNegative(value: unknown): value is number {
    return finiteNonNegative(value) && Number.isInteger(value);
}
