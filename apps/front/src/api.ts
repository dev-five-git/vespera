import { createApi } from '@devup-api/fetch'

export const client = createApi({ baseUrl: 'https://myapi.dev/v1/' })
client.GET('/users/users', {})
