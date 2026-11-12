// End-to-end check: the OpenAPI document vespera generates for
// `examples/axum-example` → `@devup-api/generator` typed client →
// `tsc --noEmit` over `src/calls.ts`, plus the runtime requests
// `@devup-api/fetch` builds from the same document (stubbed `fetch`).
import { join } from 'node:path'

import { afterAll, beforeAll, describe, expect, it } from 'bun:test'

import { generate } from './generate'

const ROOT = import.meta.dir

type FetchModule = typeof import('@devup-api/fetch')
let createApi: FetchModule['createApi']
const realFetch = globalThis.fetch
const sent: Request[] = []

beforeAll(async () => {
  const urlMap = await generate()
  // The bundler plugins inline this at build time; `@devup-api/fetch` reads
  // it once at module load, so set it before the (dynamic) import.
  process.env.DEVUP_API_URL_MAP = JSON.stringify(urlMap)
  ;({ createApi } = await import('@devup-api/fetch'))
  globalThis.fetch = (async (input: Request) => {
    sent.push(input)
    return new Response('{}', {
      status: 200,
      headers: { 'content-type': 'application/json' },
    })
  }) as typeof fetch
})

afterAll(() => {
  globalThis.fetch = realFetch
})

function lastRequest(): Request {
  const req = sent.at(-1)
  if (!req) throw new Error('no request sent')
  return req
}

describe('devup-api client generated from examples/axum-example/openapi.json', () => {
  it('type-checks the sample calls (tsc --noEmit)', () => {
    const tsc = Bun.spawnSync(
      [
        process.execPath,
        join(ROOT, 'node_modules', 'typescript', 'bin', 'tsc'),
        '--noEmit',
        '-p',
        ROOT,
      ],
      { cwd: ROOT },
    )
    const output = `${tsc.stdout.toString()}${tsc.stderr.toString()}`
    expect(output).toBe('')
    expect(tsc.exitCode).toBe(0)
  }, 60_000)

  it('fills path params by operationId', async () => {
    const api = createApi('http://vespera.test')
    await api.get('getMemo', { params: { id: 42 } })
    const req = lastRequest()
    expect(req.method).toBe('GET')
    expect(req.url).toBe('http://vespera.test/memos/42')

    await api.get('itemByPath', { params: { itemId: 7, slug: 'a b' } })
    expect(lastRequest().url).toBe('http://vespera.test/shapes/items/7/a%20b')
  })

  it('repeats array query params (style: form, explode: true)', async () => {
    const api = createApi('http://vespera.test')
    await api.get('search', { query: { q: 'rust', tags: ['a', 'b'] } })
    expect(lastRequest().url).toBe(
      'http://vespera.test/shapes/search?q=rust&tags=a&tags=b',
    )
  })

  it('sends a JSON body', async () => {
    const api = createApi('http://vespera.test')
    await api.post('createUuidItem', {
      body: { name: 'widget', tags: ['x'], externalRef: null },
    })
    const req = lastRequest()
    expect(req.method).toBe('POST')
    expect(req.url).toBe('http://vespera.test/uuid-items')
    expect(req.headers.get('content-type')).toBe('application/json')
    expect(await req.json()).toEqual({
      name: 'widget',
      tags: ['x'],
      externalRef: null,
    })
  })

  it('sends a urlencoded form body', async () => {
    const api = createApi('http://vespera.test')
    await api.post('subscribe', { body: { name: 'n', email: 'e@x.io' } })
    const req = lastRequest()
    expect(req.headers.get('content-type')).toStartWith(
      'application/x-www-form-urlencoded',
    )
    expect(await req.text()).toBe('name=n&email=e%40x.io')
  })

  it('sends a multipart body', async () => {
    const api = createApi('http://vespera.test')
    await api.post('createFileUpload', {
      body: {
        name: 'doc',
        document: new File(['%PDF'], 'a.pdf', { type: 'application/pdf' }),
      },
    })
    const req = lastRequest()
    expect(req.url).toBe('http://vespera.test/typed-form')
    expect(req.headers.get('content-type')).toStartWith('multipart/form-data')
    const form = await req.formData()
    expect(form.get('name')).toBe('doc')
    const document = form.get('document')
    expect(document).toBeInstanceOf(Blob)
    expect(await (document as Blob).text()).toBe('%PDF')
  })
})
