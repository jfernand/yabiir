# refs

Papers cited by the book (`book/refs.bib`) and by the reports in `docs/`
(`docs/reading-databases.bib`), as PDF. Copies are from open sources: authors' and university
pages, conference sites, and lmdb.tech. ACM's own site blocks scripted downloads, so ACM papers
come from course and archive mirrors.

| File | Cites as | Paper |
|---|---|---|
| `bitcask-intro.pdf` | `sheehy2010bitcask` | Sheehy & Smith, *Bitcask: A Log-Structured Hash Table for Fast Key/Value Data*, Basho 2010 |
| `basho-relational-to-riak-whitepaper.pdf` | — | Basho, *Relational to Riak* (white paper) |
| `codd-relational-model-cacm1970.pdf` | `codd1970` | Codd, *A Relational Model of Data for Large Shared Data Banks*, CACM 1970 |
| `codd-relational-completeness-1972.pdf` | `codd1972` | Codd, *Relational Completeness of Data Base Sublanguages*, 1972 |
| `codd-extending-relational-model-tods1979-scan.pdf` | `codd1979` | Codd, *Extending the Database Relational Model to Capture More Meaning*, TODS 1979 (scan, extracted from a Computer History Museum compilation; poor OCR) |
| `bachman-programmer-as-navigator-cacm1973.pdf` | `bachman1973` | Bachman, *The Programmer as Navigator*, CACM 1973 |
| `bloom-hash-coding-cacm1970.pdf` | `bloom1970` | Bloom, *Space/Time Trade-offs in Hash Coding with Allowable Errors*, CACM 1970 |
| `bayer-mccreight-large-ordered-indices-boeing1970.pdf` | `bayer1972btree` | Bayer & McCreight, *Organization and Maintenance of Large Ordered Indices*, Boeing report, 1970: the precursor of the 1972 *Acta Informatica* paper |
| `comer-ubiquitous-btree-csur1979.pdf` | `comer1979btree` | Comer, *The Ubiquitous B-Tree*, ACM Computing Surveys 1979 |
| `chamberlin-boyce-sequel-1974.pdf` | `chamberlin1974` | Chamberlin & Boyce, *SEQUEL: A Structured English Query Language*, SIGFIDET 1974 |
| `chandra-merlin-conjunctive-queries-stoc1977.pdf` | `chandra1977` | Chandra & Merlin, *Optimal Implementation of Conjunctive Queries in Relational Data Bases*, STOC 1977 |
| `selinger-access-path-selection-sigmod1979.pdf` | `selinger1979` | Selinger et al., *Access Path Selection in a Relational Database Management System*, SIGMOD 1979 |
| `date-critique-of-sql-sigmodrec1984.pdf` | `date1984critique` | Date, *A Critique of the SQL Database Language*, SIGMOD Record 1984 |
| `guttman-rtrees-sigmod1984.pdf` | `guttman1984rtree` | Guttman, *R-Trees: A Dynamic Index Structure for Spatial Searching*, SIGMOD 1984 |
| `chaudhuri-vardi-real-conjunctive-queries-pods1993.pdf` | `chaudhuri1993` | Chaudhuri & Vardi, *Optimization of Real Conjunctive Queries*, PODS 1993 |
| `libkin-three-valued-logic-tods2016.pdf` | `libkin2016` | Libkin, *SQL's Three-Valued Logic and Certain Answers*, TODS 2016 |
| `guagliardo-libkin-sql-semantics-pvldb2017.pdf` | `guagliardo2017` | Guagliardo & Libkin, *A Formal Semantics of SQL Queries, Its Validation, and Applications*, PVLDB 2017 |
| `stonebraker-pavlo-what-goes-around-sigmodrec2024.pdf` | `stonebraker2024` | Stonebraker & Pavlo, *What Goes Around Comes Around… And Around…*, SIGMOD Record 2024 |
| `dynamo-amazon-sosp2007.pdf` | `decandia2007` | DeCandia et al., *Dynamo: Amazon's Highly Available Key-Value Store*, SOSP 2007 |
| `dynamodb-usenix-atc2022.pdf` | `elhemali2022` | Elhemali et al., *Amazon DynamoDB*, USENIX ATC 2022 |
| `dynamodb-transactions-usenix-atc2023.pdf` | `idziorek2023` | Idziorek et al., *Distributed Transactions at Scale in Amazon DynamoDB*, USENIX ATC 2023 |
| `spanner-google-osdi2012.pdf` | `corbett2012` | Corbett et al., *Spanner: Google's Globally-Distributed Database*, OSDI 2012 |
| `newcombe-formal-methods-aws-2014-preprint.pdf` | `newcombe2015` | Newcombe et al., *Use of Formal Methods at Amazon Web Services*, 2014: the preprint of the 2015 CACM article |
| `cooper-ycsb-socc2010.pdf` | `cooper2010ycsb` | Cooper et al., *Benchmarking Cloud Serving Systems with YCSB*, SoCC 2010 |
| `huang-gray-failure-hotos2017.pdf` | `huang2017gray` | Huang et al., *Gray Failure: The Achilles' Heel of Cloud-Scale Systems*, HotOS 2017 |
| `lmdb-mdb-ldapcon2011.pdf` | `chu2011mdb` | Chu, *MDB: A Memory-Mapped Database and Backend for OpenLDAP*, LDAPCon 2011 |
| `lmdb-life-after-berkeleydb-linuxcon2012.pdf` | `chu2012lifeafter` | Chu, *Life After BerkeleyDB: OpenLDAP's Memory-Mapped Database*, LinuxCon 2012 |
| `lmdb-chu-hpts2015-position.pdf` | `chu2015hpts` | Chu, *HPTS Position Paper*, 2015 |
| `pillai-crash-consistency-osdi2014.pdf` | `pillai2014` | Pillai et al., *All File Systems Are Not Created Equal*, OSDI 2014 |
| `crotty-mmap-cidr2022.pdf` | `crotty2022` | Crotty, Leis & Pavlo, *Are You Sure You Want to Use MMAP in Your DBMS?*, CIDR 2022 |

## Not here

- `codd1971`: Codd, *A Data Base Sublanguage Founded on the Relational Calculus* (SIGFIDET 1971). No open copy was found, only the ACM DL.
- `lacroix1977`: Lacroix & Pirotte, *Domain-Oriented Relational Languages* (VLDB 1977). It isn't on vldb.org.
- `codd1989flaws`: Codd, *Fatal Flaws in SQL* (CMG Transactions, 1989). It isn't online.
- `jayram2006`: Jayram, Kolaitis & Vee, PODS 2006. Only the ACM DL has it.
- Books (`codd1990`, `date2015sql`, `celko2005`, `jensen1974pascal`, `debrie2020`), the ISO standard (`iso9075`), and web pages cited by URL.
- `chu2015sdc`: Chu's 2015 SDC slides. They're cited as slides and can be read at the URL in the bib entry.
