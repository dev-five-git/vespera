// Generates the @devup-api typed client (`df/api.d.ts`) from the OpenAPI
// document vespera writes for `examples/axum-example`, with the generator's
// default options — exactly what a vespera user gets from the Vite / Next.js
// plugins (`generateDevupArtifacts` → `generateInterface`), minus the bundler.
import { mkdir } from 'node:fs/promises'
import { join } from 'node:path'

import { createUrlMap, generateInterface } from '@devup-api/generator'

type Schemas = Parameters<typeof generateInterface>[0]

export const OPENAPI_PATH = join(
  import.meta.dir,
  '..',
  '..',
  'examples',
  'axum-example',
  'openapi.json',
)
export const DF_DIR = join(import.meta.dir, 'df')

export async function loadSchemas(): Promise<Schemas> {
  return { 'openapi.json': await Bun.file(OPENAPI_PATH).json() }
}

/** Writes `df/api.d.ts`; returns the URL map the plugins inject at build time. */
export async function generate(): Promise<ReturnType<typeof createUrlMap>> {
  const schemas = await loadSchemas()
  await mkdir(DF_DIR, { recursive: true })
  await Bun.write(join(DF_DIR, 'api.d.ts'), generateInterface(schemas))
  return createUrlMap(schemas)
}

if (import.meta.main) await generate()
