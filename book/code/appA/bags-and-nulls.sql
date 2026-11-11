.headers on
.mode list
.nullvalue NULL

-- 1. A table without a key keeps two identical rows.
CREATE TABLE receipt (item TEXT, price INTEGER);
INSERT INTO receipt VALUES ('milk', 120), ('milk', 120), ('bread', 250);
SELECT 'receipt' AS q, count(*) AS rows FROM receipt;
SELECT 'distinct receipt' AS q, count(*) AS rows FROM (SELECT DISTINCT * FROM receipt);
SELECT 'total' AS q, sum(price) AS total FROM receipt;
SELECT 'total of distinct' AS q, sum(price) AS total FROM (SELECT DISTINCT * FROM receipt);

-- 2. Two ways to ask "which computers went on sale in a year with a recall".
CREATE TABLE computers (name TEXT PRIMARY KEY, year INTEGER);
INSERT INTO computers VALUES ('apple-ii', 1977), ('ibm-pc', 1981), ('bbc-micro', 1981),
                             ('zx-spectrum', 1982), ('c64', 1982);
CREATE TABLE recalls (year INTEGER);
INSERT INTO recalls VALUES (1981), (1981), (1982);
SELECT 'IN' AS q, count(*) AS rows FROM computers WHERE year IN (SELECT year FROM recalls);
SELECT 'JOIN' AS q, count(*) AS rows FROM computers c JOIN recalls r ON c.year = r.year;

-- 3. NOT IN against a list that holds one NULL.
SELECT 'NOT IN, no NULL' AS q, count(*) AS rows FROM computers WHERE year NOT IN (SELECT year FROM recalls);
INSERT INTO recalls VALUES (NULL);
SELECT 'NOT IN, one NULL' AS q, count(*) AS rows FROM computers WHERE year NOT IN (SELECT year FROM recalls);
SELECT 'NOT EXISTS, one NULL' AS q, count(*) AS rows FROM computers c
  WHERE NOT EXISTS (SELECT 1 FROM recalls r WHERE r.year = c.year);

-- 4. NULL is not equal to NULL, and not unequal either.
SELECT 'NULL = NULL' AS q, NULL = NULL AS result;
SELECT 'NULL <> NULL' AS q, NULL <> NULL AS result;

-- 5. A UNIQUE column accepts as many NULLs as you like.
CREATE TABLE serials (serial TEXT UNIQUE);
INSERT INTO serials VALUES (NULL), (NULL), (NULL);
SELECT 'NULLs under UNIQUE' AS q, count(*) AS rows FROM serials;
