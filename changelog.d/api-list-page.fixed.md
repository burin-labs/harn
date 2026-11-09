- Agents API list responses carry the `page` object the OpenAPI
  `PaginatedList` schema requires, and a list cut by `limit` reports
  `page.has_more` instead of reading as complete.
