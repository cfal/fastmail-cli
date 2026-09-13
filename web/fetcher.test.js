import assert from 'node:assert/strict';
import test from 'node:test';
import { parse } from 'graphql';
import { createFetcher } from './fetcher.js';

test('queries and mutations use the regular endpoint', async () => {
  const requests = [];
  const fetcher = createFetcher(async (url, options) => {
    requests.push({ url, options });
    return Response.json({ data: { __typename: 'QueryRoot' } });
  });
  for (const query of ['{ __typename }', 'mutation { markAsRead(emailId: "e0") { success } }']) {
    const result = await fetcher({ query }, { documentAST: parse(query) });
    if (result[Symbol.asyncIterator]) {
      for await (const value of result) assert.ok(value.data);
    }
  }
  assert.deepEqual(requests.map(r => r.url), ['/graphql', '/graphql']);
});

test('subscriptions deliver streamed results and finish without reconnecting', async () => {
  let count = 0;
  const fetcher = createFetcher(async (url, options) => {
    count++;
    assert.equal(url, '/graphql/stream');
    assert.equal(options.method, 'POST');
    assert.equal(options.credentials, 'same-origin');
    assert.equal(new Headers(options.headers).get('authorization'), 'Basic test');
    assert.equal(JSON.parse(options.body).variables.id, 'e0');
    return new Response('event: next\ndata: {"data":{"emails":{"id":"e0"}}}\n\nevent: complete\ndata:\n\n', {
      headers: { 'content-type': 'text/event-stream' },
    });
  });
  const query = 'subscription Watch { emails { id } }';
  const values = [];
  for await (const value of fetcher({ query, operationName: 'Watch', variables: { id: 'e0' } }, { headers: { authorization: 'Basic test' } })) {
    values.push(value);
  }
  assert.deepEqual(values, [{ data: { emails: { id: 'e0' } } }]);
  assert.equal(count, 1);
});

test('routing uses the submitted query when editor analysis is stale', async () => {
  const requests = [];
  const fetcher = createFetcher(async (url) => {
    requests.push(url);
    if (url === '/graphql/stream') {
      return new Response('event: next\ndata: {"data":{"emails":{"id":"e0"}}}\n\nevent: complete\ndata:\n\n', {
        headers: { 'content-type': 'text/event-stream' },
      });
    }
    return Response.json({ data: { __typename: 'QueryRoot' } });
  });
  const query = '{ __typename }';
  const subscription = 'subscription { emails { id } }';
  for (const [submitted, stale] of [[subscription, query], [query, subscription]]) {
    const result = await fetcher({ query: submitted }, { documentAST: parse(stale) });
    if (result[Symbol.asyncIterator]) {
      for await (const value of result) assert.ok(value.data);
    } else {
      assert.ok(result.data);
    }
  }
  assert.deepEqual(requests, ['/graphql/stream', '/graphql']);
});

test('stopping a subscription aborts its request', async () => {
  let signal;
  const fetcher = createFetcher(async (_url, options) => {
    signal = options.signal;
    return new Response(new ReadableStream({ start(controller) {
      controller.enqueue(new TextEncoder().encode('event: next\ndata: {"data":{"emails":{"id":"e0"}}}\n\n'));
      signal.addEventListener('abort', () => controller.error(new DOMException('Aborted', 'AbortError')), { once: true });
    } }), { headers: { 'content-type': 'text/event-stream' } });
  });
  const stream = fetcher({ query: 'subscription { emails { id } }' });
  await stream.next();
  await stream.return();
  assert.equal(signal.aborted, true);
});

test('disconnects surface an error rather than silently skipping gap arrivals', async () => {
  let count = 0;
  const fetcher = createFetcher(async () => {
    count++;
    return new Response('', { headers: { 'content-type': 'text/event-stream' } });
  });
  const stream = fetcher({ query: 'subscription { emails { id } }' });
  await assert.rejects(stream.next());
  assert.equal(count, 1);
});
