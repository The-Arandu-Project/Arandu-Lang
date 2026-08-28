import * as assert from 'node:assert/strict';
import { test } from 'node:test';
import {
    TEST_LIST_PROTOCOL_V1,
    TEST_PROTOCOL_V1,
    parseDiscoveryReport,
    parseTestReport
} from '../../testingProtocol';

void test('v1 protocol accepts additive fields and preserves Unicode positions', (): void => {
    const discovery = parseDiscoveryReport(JSON.stringify({
        schema: TEST_LIST_PROTOCOL_V1,
        future: { additive: true },
        cases: [{ id: 'p::ação', path: 'tests/ação.aru', line: 2, column_utf16: 4, future: 1 }]
    }), TEST_LIST_PROTOCOL_V1);
    assert.equal(discovery?.cases[0]?.id, 'p::ação');

    const report = parseTestReport(JSON.stringify({
        schema: TEST_PROTOCOL_V1,
        future: true,
        cases: [{
            id: 'p::ação', status: 'passed', duration_ms: 1,
            stdout: '', stderr: '', failure: null, future: 'field'
        }]
    }));
    assert.equal(report?.cases[0]?.status, 'passed');
});

void test('protocol rejects unknown major and malformed required fields', (): void => {
    assert.equal(parseTestReport(JSON.stringify({ schema: 'arandu.test/v2', cases: [] })), undefined);
    assert.equal(parseTestReport(JSON.stringify({
        schema: TEST_PROTOCOL_V1,
        cases: [{ id: 'p::bad', status: 'passed', duration_ms: -1, stdout: '', stderr: '' }]
    })), undefined);
    assert.equal(parseDiscoveryReport(JSON.stringify({
        schema: TEST_LIST_PROTOCOL_V1,
        cases: [{ id: 'p::bad', path: 'bad.aru', line: 0.5, column_utf16: 0 }]
    }), TEST_LIST_PROTOCOL_V1), undefined);
});

void test('protocol rejects non-object and invalid JSON documents', (): void => {
    assert.equal(parseTestReport('[]'), undefined);
    assert.equal(parseTestReport('{'), undefined);
    assert.equal(parseDiscoveryReport('null', TEST_LIST_PROTOCOL_V1), undefined);
});
