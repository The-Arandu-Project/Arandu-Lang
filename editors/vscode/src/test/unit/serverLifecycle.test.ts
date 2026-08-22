import * as assert from 'node:assert/strict';
import { test } from 'node:test';
import { CrashRestartPolicy } from '../../serverLifecycle';

void test('restarts transient crashes but stops a crash loop', () => {
    const policy = new CrashRestartPolicy(3, 180_000);

    assert.deepEqual(policy.recordCrash(0), { restart: true, crashCount: 1 });
    assert.deepEqual(policy.recordCrash(1_000), { restart: true, crashCount: 2 });
    assert.deepEqual(policy.recordCrash(2_000), { restart: true, crashCount: 3 });
    assert.deepEqual(policy.recordCrash(3_000), { restart: false, crashCount: 4 });
});

void test('forgets crashes outside the rolling window', () => {
    const policy = new CrashRestartPolicy(1, 1_000);

    assert.deepEqual(policy.recordCrash(0), { restart: true, crashCount: 1 });
    assert.deepEqual(policy.recordCrash(1_001), { restart: true, crashCount: 1 });
});
