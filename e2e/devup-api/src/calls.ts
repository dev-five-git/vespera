// Sample calls type-checked (`tsc --noEmit`) against the client that
// `@devup-api/generator` emits from `examples/axum-example/openapi.json`.
//
// Every positive call must compile; every `@ts-expect-error` line proves the
// generated type is precise (a stray `unknown` / `any` would make the
// directive itself an error: "Unused '@ts-expect-error' directive").
// Nothing here runs — `devup-api.test.ts` only compiles this file.
//
// Known @devup-api/generator 0.1.29 gaps (valid OpenAPI 3.1 it under-types,
// so they are deliberately not asserted here):
// - inline single-value `enum` discriminators across `oneOf` variants share
//   one alias that keeps only the first variant's value (`ShapeEvent`,
//   `InternallyTaggedMessage`, `AdjacentlyTaggedResponse`);
// - `prefixItems` tuples → `unknown[]`; `type: "null"` → `unknown`;
// - `type` next to `anyOf` / `allOf` is dropped (`GardeProfile.last_ip`,
//   `GardeProfile.hashtags` items → `unknown`);
// - the default `convertCase: 'camel'` renames query keys in the types
//   (`optional_age` → `optionalAge`) but `@devup-api/fetch` sends them as
//   written, so snake_case query params need `convertCase: 'maintain'`.
import { createApi, type DevupObject } from '@devup-api/fetch'

const api = createApi('http://localhost:3000')

type MemoStatus = DevupObject['MemoResponse']['status']

/** Path params — `/memos/{id}` (`Path<i32>` → `id: number`). */
export async function pathParams(): Promise<MemoStatus | undefined> {
  const byPath = await api.get('/memos/{id}', { params: { id: 1 } })
  const byOperationId = await api.get('getMemo', { params: { id: 1 } })
  // @ts-expect-error — `id` is an integer path param, not a string
  await api.get('/memos/{id}', { params: { id: 'one' } })
  // @ts-expect-error — the path param is required
  await api.get('/memos/{id}', { params: {} })
  // Multi-segment struct `Path<ItemPath>` (snake_case placeholder).
  await api.get('itemByPath', { params: { itemId: 7, slug: 'hello' } })
  return byPath.data?.status ?? byOperationId.data?.status
}

/** Query params — struct `Query<..>` with required, repeated and optional fields. */
export async function queryParams(): Promise<void> {
  await api.get('/shapes/search', { query: { q: 'rust' } })
  await api.get('/shapes/search', {
    query: { q: 'rust', tags: ['a', 'b'], cursor: null },
  })
  // @ts-expect-error — `q` is required
  await api.get('/shapes/search', { query: { tags: ['a'] } })
  await api.get('/struct-query', { query: { name: 'n', age: 3 } })
  // @ts-expect-error — `age` is an integer
  await api.get('/struct-query', { query: { name: 'n', age: '3' } })
}

/** JSON request body — `Json<CreateUuidItemRequest>` → `UuidItem`. */
export async function jsonBody(): Promise<string | undefined> {
  const created = await api.post('createUuidItem', {
    body: { name: 'widget', tags: ['x'] },
  })
  await api.post('/uuid-items', {
    body: { name: 'widget', tags: [], externalRef: null },
  })
  // @ts-expect-error — `tags` is required
  await api.post('createUuidItem', { body: { name: 'widget' } })
  await api.post('createMemo', { body: { title: 't', content: 'c' } })
  await api.put('updateMemo', {
    body: { id: 1, title: 't', content: 'c' },
  })
  return created.data?.id
}

/** `Option<T>` → optional + nullable (`type: [T, "null"]`). */
export function optionalNullable(item: DevupObject['UuidItem']): string {
  const ref: string | null | undefined = item.externalRef
  // @ts-expect-error — may be null/undefined, so not assignable to `string`
  const strict: string = item.externalRef
  void strict
  return ref ?? item.name
}

/** String enum (`MemoStatus`) referenced through `$ref`. */
export function enums(memo: DevupObject['MemoResponse']): boolean {
  const statuses: MemoStatus[] = ['draft', 'published', 'archived']
  // @ts-expect-error — not a `MemoStatus` variant
  const bad: MemoStatus = 'deleted'
  void bad
  return statuses.includes(memo.status)
}

/** Nullable `$ref` emitted as `anyOf: [{$ref}, {type: "null"}]`. */
export function nullableRef(detail: DevupObject['MemoDetailResponse']): string {
  // @ts-expect-error — `user` is `UserInMemoDetail | null`, needs narrowing
  const unchecked: string = detail.user.name
  void unchecked
  if (detail.user === null) return 'anonymous'
  const user: DevupObject['UserInMemoDetail'] = detail.user
  return `${user.name} <${user.email}> (${detail.memoComments.length})`
}

/** `multipart/form-data` — `TypedMultipart<CreateFileUploadRequest>`. */
export async function multipart(): Promise<number | undefined> {
  const file = new File(['%PDF'], 'a.pdf', { type: 'application/pdf' })
  const created = await api.post('createFileUpload', {
    body: { name: 'doc', document: file, tags: 'a,b' },
  })
  // A raw `FormData` body is accepted as well.
  const form = new FormData()
  form.append('name', 'doc')
  await api.post('/typed-form', { body: form })
  // @ts-expect-error — `name` is required
  await api.post('createFileUpload', { body: { document: file } })
  // Path param + multipart body (`PUT /typed-form/{id}`).
  await api.put('updateFileUpload', {
    params: { id: 1 },
    body: { name: 'renamed' },
  })
  return created.data?.id
}

/** `Validated<Json<T>>` — documented `422` envelope as the typed error. */
export async function validationError(
  profile: DevupObject<'request'>['GardeProfile'],
): Promise<string[]> {
  const res = await api.post('createGardeProfile', { body: profile })
  return res.error?.errors.map((e) => `${e.path}: ${e.message}`) ?? []
}
