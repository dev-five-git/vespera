import { glob, readFile, writeFile } from 'node:fs/promises'

const files = await glob('src/**/*.mdx')

const q = []
for await (const file of files) {
  q.push(
    readFile(file, {
      encoding: 'utf-8',
    }).then((content) => {
      const titleIndex = content.toString().indexOf('#')
      if (content.trim().length === 0 || titleIndex === -1) {
        return null
      }
      const titleMatch = /^#+\s+(.*)/m.exec(content.toString())
      if (!titleMatch) {
        return null
      }

      const url =
        '/' +
        file
          .replace(/\\/g, '/')
          .replace(/^src\/app\//, '')
          .replace(/\/\[\.\.\.[^\]]+\]\//, '/')
          .replace(/\.mdx$/, '')
          .replace(/\./g, '/')

      return {
        text: content.toString().substring(titleIndex),
        title: titleMatch[1],
        url,
      }
    }),
  )
}

const res = await Promise.all(q)

await writeFile('public/search.json', JSON.stringify(res))
