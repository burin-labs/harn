CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  email TEXT UNIQUE
);

CREATE VIEW active_users AS
SELECT id, name
FROM users
WHERE email IS NOT NULL;

SELECT count(*) FROM users WHERE name LIKE 'a%';
