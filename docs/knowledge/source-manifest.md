# 源码输入内容清单

当前非Markdown输入与已发布File节点分别绑定。图freshness为stale；Markdown与产品文档另外按内容SHA冻结，不由源码指纹担保。[索引维护](indexing.md)解释差集、排除和刷新权限。

```json
{
  "schema_version": 2,
  "product": "Conduit API",
  "repository": "https://github.com/404F0X/Conduit-API",
  "head": "3f1dbb00cd6c4d6e2d3d7478591d8e24b0ec1e12",
  "input_sha256": "4c2a73a17e7cd18da0300d75c171e7dbb598eae33a6592e2e98793e79eeca3f0",
  "input_files": 1366,
  "graph": {
    "project": "conduit-api-main",
    "indexed_at": "2026-10-02T20:15:28Z",
    "sha256": "5eedb6bbd2c525fdc03eaedb3ac8e5d455289e108c655f2c2b0318a456d1d4bf",
    "files": 1344,
    "truncated": false,
    "freshness": "stale"
  },
  "exclusions": [
    "Markdown independently hashed",
    "task ledger/artifacts",
    "generated dependencies/build output",
    "credentials/live config",
    "runtime and metering journal"
  ],
  "normalization": "SHA256(json.dumps(path-to-content-SHA256 map, sort_keys=True, separators=(comma,colon)).encode())",
  "files": {
    ".cargo/audit.toml": {
      "sha256": "e7d8e2afbd853d23c32300624ac52af5cbc8d72d366d2c9e54f21161258568d2",
      "graph_file_node": false
    },
    ".cargo/config.toml": {
      "sha256": "f6a65a55703770c839ac16d357574259ba6d68af8d58d94c9a7560887d8d0c67",
      "graph_file_node": false
    },
    ".cbmignore": {
      "sha256": "79371751ad2f81f7d47defe73e701fbb7c047945109d0e65bddc1be8ed44da82",
      "graph_file_node": false
    },
    ".dockerignore": {
      "sha256": "63f0f66b13936671b16b5379bd68a129144eaa92183612426d1e47c952ba1c83",
      "graph_file_node": false
    },
    ".gitattributes": {
      "sha256": "3a91f9c90cebe235ada3140a5f5d5b0d33cf7e9bea421d1a765642722d1950d8",
      "graph_file_node": true
    },
    ".github/dependabot.yml": {
      "sha256": "ddb2a3263f05a1e1e9a4fa5248e416b65bfa2ac077437c977a4b79c798522bf7",
      "graph_file_node": true
    },
    ".github/workflows/codeql.yml": {
      "sha256": "beb7406894438449093bbc4b1afa67f11e779b42f38f2033e5d26b49c6733b58",
      "graph_file_node": true
    },
    ".github/workflows/provider-compatibility.yml": {
      "sha256": "2e927a327b26c4ba92a534eec73b6f5962944228b635910de095e3b3a67d8645",
      "graph_file_node": true
    },
    ".github/workflows/publish-release.yml": {
      "sha256": "a84751a40fa7ef9cf52da26ca5dbf3ee869148b59a39face60b7973fd1811914",
      "graph_file_node": true
    },
    ".github/workflows/release-gates.yml": {
      "sha256": "a144f5653b642880111ed9f5ae4339d4db4e77ff9963c2d0eef60d634d8bc17f",
      "graph_file_node": true
    },
    ".gitignore": {
      "sha256": "c570ada9f67051f4c02a66f0bfe3744a2976c5ba1228e272b821a06d79275eb0",
      "graph_file_node": false
    },
    ".node-version": {
      "sha256": "08062faf0d7a2d22f5d7933c50e975dd1527034275597be7c1b3b9fd2b9d079a",
      "graph_file_node": false
    },
    "Cargo.lock": {
      "sha256": "5c648953923520443d4e06408054022da1d9c10dadc43259b96ad9c55358f0a3",
      "graph_file_node": false
    },
    "Cargo.toml": {
      "sha256": "41dbf74d7418a0fe9e8a4553db9c591c66b7757539ca95ad1e8342f7cd706153",
      "graph_file_node": true
    },
    "Dockerfile": {
      "sha256": "2cb0e466a2fd89935f2b4b7a7d0642b75878d4d632123239c64f4de6f1ffcade",
      "graph_file_node": true
    },
    "LICENSE": {
      "sha256": "e12abc9b79b36045d81391db5c56651dda127fb5f7d630d834c3b6e5c121e758",
      "graph_file_node": false
    },
    "LICENSES/LGPL-3.0-only.txt": {
      "sha256": "f4ef5b7f42fcf6363e19ae31491fd7fc03a60228948dd0a4f0cc8c780fee272c",
      "graph_file_node": false
    },
    "LICENSES/POSTGRESQL_WINDOWS_THIRD_PARTY_LICENSES.txt": {
      "sha256": "6861678177de712b46dfccbb7d385011c285c78d0e84e9de459abccaed38b45f",
      "graph_file_node": false
    },
    "LICENSES/PostgreSQL.txt": {
      "sha256": "3d6af92ff8a4c2cdf69afb1cf44edea727922f5cd0cf8b5f72b11cdecac8fdfd",
      "graph_file_node": false
    },
    "LICENSES/RUST_THIRD_PARTY_LICENSES.html": {
      "sha256": "0d6f362b81af9598cb38f2c5ad66d80cd89ba19fc47578251c528b5fb24752f1",
      "graph_file_node": true
    },
    "Makefile": {
      "sha256": "17fe68b6bcf3ff2bcd58b7867323249dbf82b8967be2b0b1eb838efcde6e78d8",
      "graph_file_node": true
    },
    "NOTICE": {
      "sha256": "9cbb2c6e01c023a12ad174bc561700f589807d29982b0244b1f51ec0a341e5d6",
      "graph_file_node": false
    },
    "about.toml": {
      "sha256": "7725f62191134bccf07261b3b694edf54689497bea7725ce74f65f609c58ad1e",
      "graph_file_node": true
    },
    "clippy.toml": {
      "sha256": "27197424ebee02145e1e488c761477f4d709d228505797a05d626ba485703286",
      "graph_file_node": true
    },
    "compose.yml": {
      "sha256": "e38c496549d96c2ab8e531ca1da85d189a2c08a59c5676b6d5b47a8bb6a0fa35",
      "graph_file_node": true
    },
    "config.example.yml": {
      "sha256": "dd19c94d18dc325ca9f69150d75cc66ef13119a2a112a5983036a2529e11f62b",
      "graph_file_node": true
    },
    "config.schema.json": {
      "sha256": "85bb9792b3cdade46b08eb3d0f658afed6831d5b533774129dbdfc5e17239e81",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/Cargo.toml": {
      "sha256": "e4bcd502c9c03245241e3cc373e302ace1d86542af7f71f6935ba960d2aa8f99",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/apikey.rs": {
      "sha256": "36247fcd909dff8571dabd71a0436a733b7bf916a13f606fc06e51108bf3fe8b",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/authz_extension.rs": {
      "sha256": "a2b62bc3a7d34d23d6d7d4ae91887a68fa53634da1dbef9dd234faa572dd4258",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/backup_ext.rs": {
      "sha256": "9c771346edd801671f3d99bb4b95d79abac8460144527e3c68d1d5dda2d8fa06",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/billing.rs": {
      "sha256": "cc83d7c71cdff0efb6442ad30b263d46475deea5094fc12ffe8d68aedb76d663",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/change_set.rs": {
      "sha256": "32251c3fd7acbfcea3b13b9f5108272e7dd256a8bf316bf6bdf9e975a82f7197",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/channel.rs": {
      "sha256": "b9a0ced7a3c614c254ee2a70c89f613c701271aa99a62beeb8af7c04b253663f",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/channel_ext.rs": {
      "sha256": "f40cddbc54bf68bbb7b5faac4ce01b8dc32c17c172cf46540b02ada6bd476915",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/channel_ext2.rs": {
      "sha256": "1197f42ed02959c492002c33ca17d65bc32c38b9d890d7eaf67e4fcaf8cdb51e",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/channel_override_template_ext.rs": {
      "sha256": "421a3aec16c908bfd8a6a1b5a330b5bf4c2420941e491ad95d30067e69332e71",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/channel_probe_ext.rs": {
      "sha256": "9fdfe5b480265fd0438aaf34dd572890c9a80cca7cf897e45079d5c3311967a9",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/channel_queries.rs": {
      "sha256": "729d88c48d5988b7ec35dee84f041ee89c8fdacad0f79216ebee30e31267b931",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/commercialization.rs": {
      "sha256": "c8c4c59e18163eee11e5e67b57619237bc8d34c8dd2b216558f14413f1bf9f1c",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/dashboard.rs": {
      "sha256": "19186ab7713e6a75e5b19f3cb4ea06af9dfe246d815fb34373fa1fc30920e64d",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/data_storage.rs": {
      "sha256": "1223b7b7e3367f6f9bf345d460ca854819f62efbab14f3a0cb27e0b3a5c017f7",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/dataloader.rs": {
      "sha256": "a3cb14ebf41efa0b5139be41b8312e28f87a20e815debd69e7e368f2a968c90b",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/input.rs": {
      "sha256": "cecfb921fb5776deed62ffd1f26b54f7ed1caa7e61a23296fdef4ff6e45fce37",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/lib.rs": {
      "sha256": "8604529290902743b2677f2558eacf8621a0fa16ad239047ef9ea3acdd2f1a70",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/me.rs": {
      "sha256": "6d2ab488349c89b16b1c827486e281712649ab531408b238e17bb8bd3dca10dc",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/me_ext.rs": {
      "sha256": "84bde5e761e399fd1161971fd5f99eda23c37e45158daa1a5a140e44fa6088e2",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/model.rs": {
      "sha256": "5ab0aa530b3bd9de36d48c7a44c326dc7c6f40e933fb9d1dfdf3a75a6e3c220e",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/model_catalog.rs": {
      "sha256": "b07fbf5d5b9bbfcce7834228833931eda7e2023f61db9dce1af6c9c7679c3d2a",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/model_ext.rs": {
      "sha256": "e5c6a95aa6f8d0366f9fd331e46f087fbb750255034b9924025df250413bd986",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/mutation.rs": {
      "sha256": "3c123c1489881e1c29547ed650a10656cc9fdc027496d20a2b2f00a21e3d5f3c",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/node.rs": {
      "sha256": "54b628d877a92bd6668f9c97ef239ea40d83a9a96ef6587e83742a6edb0c7f5d",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/operations.rs": {
      "sha256": "0593e83be3512c6d8fbab97a5ccb2f96b9a1bb17f444724f2e4d8e1f4aa261d6",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/pagination.rs": {
      "sha256": "44b46875849ef210f5898c9b7e35b82fec21f194ab132d3b91bc347fc20bda23",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/policy.rs": {
      "sha256": "3bc3fb968f9e80bc8480750db63ab3dc20997f3c791871d9336d47ecea8516bc",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/product_experience.rs": {
      "sha256": "d635d2aaafe60b19c88a8dae4ee63e0bc172bc7d3d09e18d3c34d72b755e98ee",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/profile_template.rs": {
      "sha256": "86b3ec1cfde37d2925fbe76881d7e5b4eae01c528989d11a317c924dafbadf21",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/project.rs": {
      "sha256": "bf045896c0ab66092cab6cb9327b3bb0575bcfb3fccbff6292d6dac1b67bfc3d",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/prompt.rs": {
      "sha256": "0ec76f133cc9f9dca564d0eea6af4ba49cab0303a135deda0b35a751badad036",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/provider_quota_ext.rs": {
      "sha256": "bd0d9ffb7f9dddd799a7e3759763f3c28c99f06b72936a3d9e941668ec5bc3a1",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/query.rs": {
      "sha256": "79a3425da9c4467734a140aaca83b9dee6f3d2e647420cf7a00f93889bee21c0",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/quota_ext.rs": {
      "sha256": "45594d31b6f6d9513a6054536c56b6b392c43bf9c0012dab77244b8f17b05aa6",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/request_execution.rs": {
      "sha256": "0954908a12eab46f32b96aedd68570afe20beb3dbf4dbf6902c97b831684c9b2",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/request_usage.rs": {
      "sha256": "cc52399b72764ac1906a65129f3823903e369ad863b72a5fc2d2b25b55b586d7",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/role.rs": {
      "sha256": "fe0a1499f615142541502d44093ba5bc4856409d58ecaf2b863522607bb38bf0",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/route_explanation.rs": {
      "sha256": "10053bb0b0319788f24691f3b16e70a88db31c6120dc9ab651358d388c6f23dc",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/scalars.rs": {
      "sha256": "a2a270d814fd1565a245ef88752f26d795cfaf26557d710bf28b966543815ab5",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/schema.rs": {
      "sha256": "c55088773d4fc54caa855d437cd2a6b446af2eaaea0ea41a105d107ab2776acf",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/sdl_parity.rs": {
      "sha256": "58d0203ad8a1a51c5a8a2042a9ed395766609054a1bf420f8152a3412c3f5def",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/simple_group.rs": {
      "sha256": "ece95bb0a60c6732a32e84b47a96935fcc990656fc59be31c89cdc0f3bb04312",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/system.rs": {
      "sha256": "620c730804394ac98e12f74e650ec83de00b9f81cf71b997503700dfda7c6080",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/system_ext.rs": {
      "sha256": "ae7a0e6670ddaa1ee58c0ad33afd036c836f682723e4bcad67fed81add6433f8",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/system_operations_ext.rs": {
      "sha256": "764ecd80e5a670dac9a44587eea49dacad0be37027e6dfba2bf50e65e4b7ba1d",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/system_settings_ext.rs": {
      "sha256": "03ff7f398f0c4e5d64653cec7f8f70718728f76f77ab4c0bd0fd9655048cb463",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/threads_ext.rs": {
      "sha256": "15a413b7d122b21c09ac0783a87063ad4e7cffd1c366a3bb29543d89d17e0836",
      "graph_file_node": true
    },
    "crates/conduit-admin-graphql/src/user.rs": {
      "sha256": "a101e927765caa75400dfd0d472d2b6d60a1f447bd42a2965e54287a94a9a335",
      "graph_file_node": true
    },
    "crates/conduit-auth/Cargo.toml": {
      "sha256": "ee9122d8bf1becdf7f8ff07033359b8f8f4baad12d339d40b17dac9c6fe9d25b",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/apikey.rs": {
      "sha256": "39ba8af60a0ac4ed56f9bcecd4e540d9368d2871b819a5feed07d0bb334afa2f",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/http_auth.rs": {
      "sha256": "3aec0d317aa0df456d691e94cabd35690253f2fe2cea6ebf27dfbc77b6065704",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/jwt.rs": {
      "sha256": "2a06e8813f529c4d6b0a9764e0ffdb60c1df166e6bee7f98b6c3da6f03a8f0b0",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/lib.rs": {
      "sha256": "b32b4f0b25101c5d35e3a8233b93da5db4ee98cd2c0c0a5c23c7b6249b5c032b",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/password.rs": {
      "sha256": "1e27066f3e5d002b65c63a76327cb93b2e6545fe18379c2f0f528b433b3be2ed",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/policy.rs": {
      "sha256": "a1f225987b287c82b180b67f11009cf9cdd0697c240e4b56ea8bc6e5977dbb7e",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/principal.rs": {
      "sha256": "a49eb852c6fed0e4ef9cf0955a2e2bc9ac79f1b7c0a51f3a54683d9e2076bbcb",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/rbac.rs": {
      "sha256": "3d003bdaa8062285ca5c6f5b8b47a5149f5358b75a8de1374484f96615d3540c",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/request_context.rs": {
      "sha256": "d1896b491e31f04172bffcd06d7b87ffb51cbc35aa6476913de4b7a311366256",
      "graph_file_node": true
    },
    "crates/conduit-auth/src/scopes.rs": {
      "sha256": "2b6cf8e7bdb0796a947f646a1f332eb71438773428475b9dac306a39e5a84f16",
      "graph_file_node": true
    },
    "crates/conduit-bin/Cargo.toml": {
      "sha256": "1e27f3fdcd60b27f8858a43454ed2c6765bcd12d082c4163929365596540e159",
      "graph_file_node": true
    },
    "crates/conduit-bin/build.rs": {
      "sha256": "c5e4933500f529ff34bd7ef80d8478743e30f31195db77f3a5a19692d66a3397",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/artifact_cleanup.rs": {
      "sha256": "fb46d4d0daa99dbaf156bffe6e29bd3c919b2944c5ffb6ca0d19c175a4318207",
      "graph_file_node": false
    },
    "crates/conduit-bin/src/auto_disable_runtime.rs": {
      "sha256": "a95f04ac73085b3883a01b06f9d24192219ee5bc77fd4980d8f4a9fbeda1c788",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/cli.rs": {
      "sha256": "45fa32b53190d3373fd5df9ab8c2fc4eab06e16a08b0165c8bb7ffd1aee2ae9c",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/conv.rs": {
      "sha256": "b66632afb483001407496c710ae5e744dde7cd34087a3f1327d4a19427f03abb",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/embedded_postgres.rs": {
      "sha256": "bad7497406e392f470d862bf9f639ede6969157f7c54a32580b92bae0c5743d4",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/main.rs": {
      "sha256": "532dd49b046c575db4f4b523275815ec18d015734b14912a984ebdf279e627cd",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/maintenance.rs": {
      "sha256": "2870b54b350cc873363bb4ede429a40a6ed309628ba09520819af5d752ad8eb2",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/maintenance_claim.rs": {
      "sha256": "0f332c819df0147829e1ded7d56c08843489ae9ad58b2a0db18ca6ffb8be1069",
      "graph_file_node": false
    },
    "crates/conduit-bin/src/model_fetch.rs": {
      "sha256": "125a151b815abdb45a47f1cef2ecd21d223f833e4df971ce1cf8cdfd6fe97cfe",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/model_matcher.rs": {
      "sha256": "e16e80538cfa8c289e154d74db1cc5fbc2b497dddb78185e827955c6ca96fa4a",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/postgres_test_support.rs": {
      "sha256": "737ecede0b23738c4c70faeb8e2383c180b93103d3176102b7116d900062fc77",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/repair_regressions.rs": {
      "sha256": "1e3d527734cdefa9aaeafc5b981197efcf925b0844d902d4c50a94a6ae5f4e64",
      "graph_file_node": false
    },
    "crates/conduit-bin/src/route_affinity.rs": {
      "sha256": "1dd905ebde42fb2ecf2c384df7aa911499b9fe4cd7114f15ff481fdfea952437",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/runtime_logging.rs": {
      "sha256": "51d376e4d76c41a2cb3f44134893267b01a30a52b3b73488497de2ba0ea8faf2",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/usage_charge_settler.rs": {
      "sha256": "f082cbbc36276e8c421373dc20f7920d14dc66544f282deec21903a22ffe666d",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/usage_charge_settler_postgres.rs": {
      "sha256": "6ddbc2102b9a1bd2738a22e7f9c29eae7f2123c7330f6365891b1a6a849cde63",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/usage_log_recorder.rs": {
      "sha256": "3332ec1384c0d73eb447093507cb07041301249b77e0e4e09ae71efcc2294e53",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/usage_recovery.rs": {
      "sha256": "2be5068c7ce7e4ab66a2f4e13910a4bd5734ea50a9c9d3bfb4ca3301bceb69d9",
      "graph_file_node": false
    },
    "crates/conduit-bin/src/wiring.rs": {
      "sha256": "d4e3f7c8a6407a1eea8e4c75c49fd6e333cabc974f6eb652cd0261dddfd7a866",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_apikey.rs": {
      "sha256": "7a0a75c71d10ed32009fe6e8998f9ca17a743cae92c5b6048f76c82600104cf6",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_channel_crud.rs": {
      "sha256": "fde198815b0e4e3c0f8e776f5552d5236d381df009d7161ad5b9cf7aa0b9d14b",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_channel_ext.rs": {
      "sha256": "db31df92cf5bfe21813168b2f7e9dd1edc6ce7dbd7787e02fef3a8099a80375f",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_channel_override_template.rs": {
      "sha256": "2a385b0ea31b667b985668553daf3d71c475aade46d9c071adb43c2c3ff57d4b",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_data_storage.rs": {
      "sha256": "1d50fcc05ca7783d8a9e5e3091c25d65e8fec23065dafb4dbafe859b166eaf35",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_model_catalog.rs": {
      "sha256": "77ff894bb6df475aa534deecc1b7949c79198f623cac8fd349d4a92b3b8bbf18",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_oauth_admin.rs": {
      "sha256": "e271ba922e1ad9098694f4d022a73a8fac12a6a380f338d66e65cf279fd8cd36",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_oidc.rs": {
      "sha256": "583e9f37ca08da7e7821614bcbe0da688fd694fa375beb40b165df70afa0e012",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_openapi.rs": {
      "sha256": "dbb157e4aa8e0e863fef6f50e9b9bafe6d037a490fd3b994d3170f0a3d72166d",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_operations.rs": {
      "sha256": "8ea92b70fffe949130dca619708f0d4843976e29f659ef097b5ac4b781a0bdab",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_auth.rs": {
      "sha256": "e6af507cfdb60b251f888492a8bb8985b6f242f1d8a22868429173cae3f0a481",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_backup.rs": {
      "sha256": "c3d04d898f6365bdf6151e46986a00be0f04626bad71d7f651148a863ac84e3d",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_billing.rs": {
      "sha256": "825d5910e5f7a6d1b6af006ae393a96c48ec3391dba53ac5a26599555cb68008",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_change_sets.rs": {
      "sha256": "6fb6e6df315396b97778f2274cb04182bfb79b6d133b7719a1881a0b0cf753ac",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_channel_model_sync.rs": {
      "sha256": "b9272f17d9f468f57210f97fed8dd296eac47c5b59449a6ad4e6cc129c5eba30",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_channel_probe.rs": {
      "sha256": "34a104060792775a2c58cd25da4926c13dd72abb3e60be7e41865a32a1445646",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_channel_probe_query.rs": {
      "sha256": "ef76d404c9a2f25c2524833e7e4867a046cad6d7ad9ecc3dc1b20d4d0c0d1805",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_commercialization.rs": {
      "sha256": "c36fd28132b2894d84c551e50eca5d1048af44d61f9808303a8191cf13fdf4c1",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_dashboard.rs": {
      "sha256": "de69e34d18a98d5fc299254fbc3b3cb6f6dc129260e985f7d1c84f3ca9870abc",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_identity.rs": {
      "sha256": "6abe938fae3c793a1fcd70eb5b4191cfe17f7eb03e20dbbd8e86353363bf460b",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_model_market.rs": {
      "sha256": "3888ec7fe856d4bc8dcd1b9aeaffacd494c91a63ee7bd61f778ada7ea41019d0",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_observability.rs": {
      "sha256": "f8c8c4921cc209c48a54ed1fc39f535a96e388e020dd704c551b996629e0c38b",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_openapi.rs": {
      "sha256": "aa110fd6fe94f929a246d6699676e50d76a8b25b38e660bddebf9afa42218236",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_operations.rs": {
      "sha256": "d09e24120fad99cee7c2c9b7c55b3604ec28fe23eebe84201e7782571a208cde",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_pricing_admission.rs": {
      "sha256": "5eedf799fcc19325ef4361dad925269402b071c7837d4ad5847cd572124e8355",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_project_role.rs": {
      "sha256": "d8e63a0a780b1b27464f2747494dbe83d66763690ef155df30316275ab8b9056",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_provider_pricing.rs": {
      "sha256": "d402472b1f434f1a3445cafcab6733c34d9bf6e21f37cae113c15e4743e8b028",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_provider_quota.rs": {
      "sha256": "721a54a575a76746e6d88bfd1b0c642851ee0674dce3461c72256815a88d4ce6",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_quota.rs": {
      "sha256": "8cf31784749b2a110bd2cd1fcef9aae805f0fe9c2aff388bbde96bafb4af8217",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_redemption.rs": {
      "sha256": "dd8fd5a10968ca84feadb67de46f4d4d1f4a35d2e665f7b60796b9470532ad65",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_simple_group.rs": {
      "sha256": "77f7636f417b625b181a601105e0261cac606732915f446035547a67f6c43ad3",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_system_initialize.rs": {
      "sha256": "5ec7bbdb787b63d1b9216ae2ce92271f289838100aaca05852e82773d2b5cc40",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_system_operations.rs": {
      "sha256": "a5b0d9bdbd4ea450c284ce926a3a580d2f480637a2f7a6ee8de2aa77ad2bfad6",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_user.rs": {
      "sha256": "574a766f31f16ef4a27eef0abb392c28e2bb598d931c74540355241982097cb2",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_postgres_video_storage.rs": {
      "sha256": "6109efb9679a223f45dd31aff1ab3b25010691882de18b747a10c06c9f979276",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_product_experience.rs": {
      "sha256": "2ff1671899b03ab7f9b3e2f2b14dcab76050da0699f0868a433a13c442be4f48",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_profile_template.rs": {
      "sha256": "5e9ff511f20ccc7e5bcca96f4e15400fed89d7c00c17e184aecb9142abcd0534",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_project_access.rs": {
      "sha256": "5a60d06cfe36c541b3b1c50f424707fcaeb4d2ccdf2be3cb957e174d41d2487f",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_prompt.rs": {
      "sha256": "d32a1496d50f8f050e373b62418c3b6d79be86f3515f3c489e74b36e89ddbb7e",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_quota_common.rs": {
      "sha256": "49f0424655211f8c384060d0f3932e0f4f2a46dfaca1f775fc50aca8758157fd",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_request_content.rs": {
      "sha256": "02f2f89dda70fc6bbe61e24c8e310157ba06a8211dc208cfb45659cd68577eec",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_request_execution.rs": {
      "sha256": "6b993f04b5539dd56ece6e194cd3b2241bed86eb16f60f92ca2a9d5eca8bba05",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_requests.rs": {
      "sha256": "1481fa89bb22696c790be28a2096c29457b16d9e8e94df34ca158bd52753cbfe",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_route_explanation.rs": {
      "sha256": "7cbb014550a3d061681701a46b703669ce0d9e651709587fbab7167fb3937e7e",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_route_health.rs": {
      "sha256": "afd3636d8f0f58b207fb254775c9c45708a2e49e6720b82578c71207d2ae8895",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_system_settings_ext.rs": {
      "sha256": "431045ad6eac167b8ac60fee579ef51dae0afdedde64384f7c570f474464c259",
      "graph_file_node": true
    },
    "crates/conduit-bin/src/wiring_video.rs": {
      "sha256": "678aade50b409e99bc192b542f5f1d1481171330ddb2b2b01ce41489ff3fc7c5",
      "graph_file_node": true
    },
    "crates/conduit-cache/Cargo.toml": {
      "sha256": "c7dabc761609fb85a63a82215c9d0ff2da1c6914a55b8cd6173f5e8504c24b79",
      "graph_file_node": true
    },
    "crates/conduit-cache/src/entry.rs": {
      "sha256": "c215f62482282db7146575f6d01e1f77f696d64eae2ad18314236e583bccf5e1",
      "graph_file_node": true
    },
    "crates/conduit-cache/src/lib.rs": {
      "sha256": "e04aaefc91798e4fac229130450645f0fad11fc327a1b77f657620d30a147c55",
      "graph_file_node": true
    },
    "crates/conduit-cache/src/live_cache.rs": {
      "sha256": "c284a7d64f5a0b0b573fe5acd3a721720030bcf23a51026753c777a921ba8bd5",
      "graph_file_node": true
    },
    "crates/conduit-cache/src/memory.rs": {
      "sha256": "70df27adf97684e8ed9eb72b349799a3fd527a1adfd066417ef503564271b967",
      "graph_file_node": true
    },
    "crates/conduit-cache/src/noop.rs": {
      "sha256": "266efeef88c27ff9293013a2c8c202f9a63aec44526766d81a169fe4cd676c4c",
      "graph_file_node": true
    },
    "crates/conduit-cache/src/redis.rs": {
      "sha256": "5307be2230b86f7a21a7f448809f5259320235d7bade5d8bde5f6573c5995ac1",
      "graph_file_node": true
    },
    "crates/conduit-cache/src/two_level.rs": {
      "sha256": "aa471d6a037adcfeff2ce859944b59503428250176a246611a569916a067aff2",
      "graph_file_node": true
    },
    "crates/conduit-config/Cargo.toml": {
      "sha256": "69e6136c48d0d654b1b3854813a8d4bba055e831024af53a33763e52a2aa5c86",
      "graph_file_node": true
    },
    "crates/conduit-config/examples/extract_defaults.rs": {
      "sha256": "e3bf0da72c72ac3ec8c225f5ab50b449c1199cefb614677ff75b0c62ffc3eb39",
      "graph_file_node": true
    },
    "crates/conduit-config/examples/generate_schema.rs": {
      "sha256": "7e079d9430c51be47f15648d778b57f0808322348d630fe5282cb1d8114dbd6b",
      "graph_file_node": true
    },
    "crates/conduit-config/src/export.rs": {
      "sha256": "8f1440ac119556d636f9c64efa95c661923e47e0a4fb4672784ba346d54dea09",
      "graph_file_node": true
    },
    "crates/conduit-config/src/lib.rs": {
      "sha256": "96145a7063bdc30333884be226b8db67f01d791b0e1637b48b3c1b8d2e2e2859",
      "graph_file_node": true
    },
    "crates/conduit-config/src/loader.rs": {
      "sha256": "d581ec23e3beb5d29e0e036dd1b44958fa0bacb2c0fce53c75094d45996fc113",
      "graph_file_node": true
    },
    "crates/conduit-config/src/model.rs": {
      "sha256": "c7ebdd6a347afecea7403a47abe5a67182769abf5254e7c9e627885a29d1c4e9",
      "graph_file_node": true
    },
    "crates/conduit-config/src/schema.rs": {
      "sha256": "628f5ad548a2574ce0a2960fc74ac03ed81379d3ce10d5240858ab488951f3a9",
      "graph_file_node": true
    },
    "crates/conduit-config/src/validate.rs": {
      "sha256": "6cba7ff78acd98c49c265e9310092afb459997509d4a77e6d2f50a9073b9d0d3",
      "graph_file_node": true
    },
    "crates/conduit-core/Cargo.toml": {
      "sha256": "41b60c2d6c39fc1d64aedb7d78614ec07451a95f2e521648028cb7f991c7115c",
      "graph_file_node": true
    },
    "crates/conduit-core/src/error.rs": {
      "sha256": "c4fc3cf720a76012c59bc8f9347b82b37f936a36b2f966c5d5d2dba1aba66f70",
      "graph_file_node": true
    },
    "crates/conduit-core/src/lib.rs": {
      "sha256": "b82f88de1d669b19ff11538dde7ca28871d3506efac8473221d7c6bea755aef1",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/apikey.rs": {
      "sha256": "cbdcebeba3f35da3439b0a5bf18f196e610e36138e3cdca7133fd5d570bed98a",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/channel_settings.rs": {
      "sha256": "2e4558cbbfeed71a554d0e8fb0cd50f1009fbaf858b294e686034d6a2536f3d8",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/condition.rs": {
      "sha256": "3e043647c47aebe8d7ec24d8de796425d4026514b8a0637130bb15c78bd14dec",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/cost.rs": {
      "sha256": "80bc60b901fbc4984cbf1e66c6123d0f42f5ae5f2346278837682991037c80b9",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/mod.rs": {
      "sha256": "e2e0363699041628b1e2829678cdea96fde09d7fcb34e66553efe3736649d94f",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/model.rs": {
      "sha256": "913622f2edaa60d3f619f1bcadcdb1fd64c755eff62fd5206d41dd3b3581d28a",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/model_association.rs": {
      "sha256": "b4f09695400deeb11292eec4cf605e1b84cfa27ee8b477d8695daeead56f8e3d",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/money.rs": {
      "sha256": "3d3a621262e4cff9d2aee674961271605ae63509fbd4ed5a1b4f2bdda972e87d",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/overrides.rs": {
      "sha256": "f71e0090e920137796a47eb75ac78d0926faea08d991b285bf8fb10ffda1d105",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/pricing.rs": {
      "sha256": "b95142a355f32d07b5be75e0328c9944d91bb8ce78ecf680e157f488de878259",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/project.rs": {
      "sha256": "a2adff3c55df90ee4d2fa3f0839a776e8e713ef0cc7ed328d051968fa57a338b",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/prompt.rs": {
      "sha256": "405fd0237c994cdf4e241b049aca2e305dc0f23cf5f1ee5f3ddac3447544026e",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/prompt_protection.rs": {
      "sha256": "fad36fc8448cba2a86f7e070c5544fbda72d855abea979ade25cd40b081e2e3d",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/response.rs": {
      "sha256": "7cd0a433956cea684cce7a1fe72542f55fccf944d63c8a5f729ff4323cc38e3b",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/storage.rs": {
      "sha256": "1752548ba9f3776af2edacee5f0e2112f4007897b993a81e2dffc3ee5c9698ee",
      "graph_file_node": true
    },
    "crates/conduit-core/src/objects/user.rs": {
      "sha256": "125c842e93cd158fe82a951694a03c69440d7719281ac489ace86384c7d737c3",
      "graph_file_node": true
    },
    "crates/conduit-db/Cargo.toml": {
      "sha256": "6f7272abb147f074936c3ea87190152b5e83b1e2a323a7986d37bd1f4805819a",
      "graph_file_node": true
    },
    "crates/conduit-db/src/connection.rs": {
      "sha256": "637a779ba3fc381311dc812135558e2e25c74e7ccb30a335fc0ad268b2e7aa3c",
      "graph_file_node": true
    },
    "crates/conduit-db/src/lib.rs": {
      "sha256": "14160b5e1117bb2937a492500bb607a718be7d18845622e87fe4fc4518f88d41",
      "graph_file_node": true
    },
    "crates/conduit-db/src/migrate.rs": {
      "sha256": "a5ffb78218f19700040342f54830a1d9c0cc628ca351d705afd1f0916e5a24e1",
      "graph_file_node": true
    },
    "crates/conduit-db/src/pg_quota_admission.rs": {
      "sha256": "af85484af18ec0e753a4fb44790963d0aae32fc37939443269fa81f8485f8de9",
      "graph_file_node": true
    },
    "crates/conduit-db/src/policy.rs": {
      "sha256": "a7b3a92478fb41ad0076dd4906e10f2c1df5a041f01fc2f1444931b4a0240b41",
      "graph_file_node": true
    },
    "crates/conduit-db/src/pool.rs": {
      "sha256": "35ee2e9ff9d698deb36a0528259a219aa25f0d6d5d659339c62e25d40ef14927",
      "graph_file_node": true
    },
    "crates/conduit-db/src/postgres_test_support.rs": {
      "sha256": "c929dafd6d6ff231c661a61ddc64f15c6c0d4ace07b9f96d07f551f3b7344045",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/api_key_repo.rs": {
      "sha256": "699dcdf8399b2ef7795508608f52ee5c802ff2cdfe8269967df540a67f5e48c9",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/channel_model_price_repo.rs": {
      "sha256": "acbce899502e814de32520e9d1dcdfe136495351bb76934f26e76f48aa7a96fe",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/channel_override_template_repo.rs": {
      "sha256": "6df099bd56e858cf1709ce48f515665758789c2bfe381358c809cea30d9a289e",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/channel_repo.rs": {
      "sha256": "a8cf8e251644aa135f21c10c62eef4ead49ac48e49247a8627403333e3d1dd50",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/data_storage_repo.rs": {
      "sha256": "f2d407cf271acea4dcd2d72b3cd4643ddfb5d66644e18fedcd2971579dd2fad0",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/mod.rs": {
      "sha256": "4cd215bbbc1908d7656a53b3aba6e2f5c304786ab566f9362ec565e9049dcb8b",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/model_repo.rs": {
      "sha256": "0aee076405fc90d5cd1eaac40c6d25f56c6a6ca82f70ee5c25fab1b398d4f438",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_api_key_repo.rs": {
      "sha256": "93352668adc39da11f6699757ee626d8c0ce050bd034ec90c7db4a7441066102",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_channel_model_price_repo.rs": {
      "sha256": "d97f85abeb244c186a162313859a55da6710465c3a05d89223dd8cde12436ff1",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_channel_override_template_repo.rs": {
      "sha256": "c6dbe637df909a38d9e56b127377f9544e29406848114b3363c1f6278eaa8bd0",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_channel_repo.rs": {
      "sha256": "4db25758a5ff6a2fd058e0163db6ad4efced8a41455451dcea88cc353331686f",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_data_storage_repo.rs": {
      "sha256": "f5fd40160be139ea171a0623d8ac84769c86408257418d08055711580189f70c",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_model_repo.rs": {
      "sha256": "80050f4f6f225076d1ec09e51f95fe8432322e0b98f98b5494e094ce084a0139",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_oidc_repo.rs": {
      "sha256": "af16f04be5ba59ec3b3cf26a9320901687f40bd1644c9b235f9a4293ec945843",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_profile_template_repo.rs": {
      "sha256": "3e497d9ade971fb97918c668fed07a17ac7edbf1b7a99c27b99d5706ec9b5d1b",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_project_repo.rs": {
      "sha256": "c6ea075b268ae9ad4dfc3342b3f8f1e5d43c0a60cb047953e0a7c4338d7c2daf",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_prompt_protection_repo.rs": {
      "sha256": "ca6b32d9916fce06c41915711c81d686f23fd62f16326d606fd81ac34c86d360",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_prompt_repo.rs": {
      "sha256": "217dc23c1b15876d4989b2007dc3f31dde0a849dd34226e3e58781d892b91d12",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_request_execution_repo.rs": {
      "sha256": "d68416ae2a21cec897bffcc4c443ee561957ea11b7cb3a210bd05d36f98f417d",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_request_repo.rs": {
      "sha256": "cbe24b45f3278f6dae78ed7d9e975e59726e977e80892d559d36dc96baa7cce2",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_role_repo.rs": {
      "sha256": "426b0969a2ea4f58a3c120131067ecf3320cad48bf5627e9f5d141c0647e165c",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_route_affinity_repo.rs": {
      "sha256": "a23b6344272847983d534acbcb5185e0cea65d3a00f25fcd8ccc7faaf4033561",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_system_repo.rs": {
      "sha256": "99a31a1e8274c74701400981fbce7c23165578f0f98e257c5c8161b3673c22ac",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_thread_repo.rs": {
      "sha256": "43e7483932c83dd9b9c915765fe0f81c82ac9f400d65ad70da16de17bd7b1b23",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_trace_repo.rs": {
      "sha256": "de84ddf2de9b8ec042a2ec9588177cc75a68be44d23519c68f6bbc332bd88fe8",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_usage_repo.rs": {
      "sha256": "6fc84c01e2cd67e9765fda74dcacbee9e63ab21cc5646d9760e44fd4aa456e7b",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_user_project_repo.rs": {
      "sha256": "8f2180ce277bc61c3a92a46971fec9ac536315ad3669beb26cccb566ebc714fc",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/pg_user_repo.rs": {
      "sha256": "8efe3ac87b20089bc2dca7bc24b2f959ac98f8d6ada88b8882449e31151d5629",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/profile_template_repo.rs": {
      "sha256": "76ffa9ecb499b5e1c7a3167d264cf37c39bbda923ab1c5776b6e76b6171e45ae",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/project_repo.rs": {
      "sha256": "fcb714bf7b9ba72b92013837b9f2ed5b1bbf41fbb72e4ca1596da221100225be",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/prompt_protection_repo.rs": {
      "sha256": "9d85a87eb5756e8171ce94dda79b96b1eac6f9f9623108efe224ce300285308e",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/prompt_repo.rs": {
      "sha256": "de6a9212406af2aaee335198ce65f2b00166ddfe94230a5c9cf7bf015ecfc70a",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/request_execution_repo.rs": {
      "sha256": "2a8de6bd354e4afcf4447ef7c050d330f4601d1447141b1413585da81bed0c80",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/request_repo.rs": {
      "sha256": "d0c574d7a8cedb20394e842c26a860cef171e82195f6745957b84d26a374e376",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/role_repo.rs": {
      "sha256": "22704adb289f73172d86a74a58576552a6c5d1bc7c7242a340664a0a187886b0",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/route_affinity_repo.rs": {
      "sha256": "4748ce280eb1e41633998956a506ee7c0bc1a15920e6ef17821cded6a191c486",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/thread_repo.rs": {
      "sha256": "0b41aa9e2c954dd3431a05102080b431d9baac3522310d3bc2a96b9eb11a5c3a",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/trace_repo.rs": {
      "sha256": "1e046d8a7e08e99f2026be37c19830ea027341e6889580250953d74b787d1048",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/usage_repo.rs": {
      "sha256": "e7ede9596b6d006970bcefffd17255bc929c4ae77f38e5f4e4a47f01d15a2aba",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/user_project_repo.rs": {
      "sha256": "8e901c5cc220222cb40ca54df64ef34c548fed41e1569e4140272a9bd1e65f2a",
      "graph_file_node": true
    },
    "crates/conduit-db/src/repo/user_repo.rs": {
      "sha256": "0df8ef9d770a0b72e2a5b5f263e4143506f1fe5dbae29bc44a94621feef8586d",
      "graph_file_node": true
    },
    "crates/conduit-db/src/row/mod.rs": {
      "sha256": "2c08f3d5a91d3b5fa634baf538c440185c9aff1e7714d34e8643a008fe2e8003",
      "graph_file_node": true
    },
    "crates/conduit-db/src/tx.rs": {
      "sha256": "85097fc8d8827f98a354c5df58e912f3131a3e13f146e8f4451ae1ce4a2e8453",
      "graph_file_node": true
    },
    "crates/conduit-http/Cargo.toml": {
      "sha256": "91cc21e7c542f712b78969c9c049a20b0995febacf59c21cb6511a35229f7823",
      "graph_file_node": true
    },
    "crates/conduit-http/assets/favicon.ico": {
      "sha256": "d94482f7a3488a593a93a3ca37ea0b0a9d78a1ed83bcbd8e21037f855df7f978",
      "graph_file_node": false
    },
    "crates/conduit-http/src/admin_handlers.rs": {
      "sha256": "f6593ba9f8cf37fb6f6139dd2cb7db7bed84116386d06c583e353ab09e397356",
      "graph_file_node": true
    },
    "crates/conduit-http/src/aisdk_handlers.rs": {
      "sha256": "2548ab9ba6bd64c1966d53721f7f0ea5028c73cefec7118b2d303137e0445870",
      "graph_file_node": true
    },
    "crates/conduit-http/src/anthropic_handlers.rs": {
      "sha256": "42cb5f2636991a021e6611fb4ef6770ff67704d573e5dccdfaef18e9eb994c40",
      "graph_file_node": true
    },
    "crates/conduit-http/src/api_error.rs": {
      "sha256": "aa263d9a1a092b4c79ccb8b14e3239c45d9a0f4315a4921dad3b408fca30ea34",
      "graph_file_node": true
    },
    "crates/conduit-http/src/app_state.rs": {
      "sha256": "5cecc6ffac58bd3982f4e1dca04fdbb6fa4740e9dbee58522ab89a58c198300b",
      "graph_file_node": true
    },
    "crates/conduit-http/src/asset_source.rs": {
      "sha256": "709eba0e5b3788eaea7b267a6960eb8ae1c2ed10f52988dd22562cbe159a0d8a",
      "graph_file_node": true
    },
    "crates/conduit-http/src/auth_handlers.rs": {
      "sha256": "214a07ad8c7a9377888eff35706d4f319876f4e2edda5677d51b5e061588d0f9",
      "graph_file_node": true
    },
    "crates/conduit-http/src/doubao_handlers.rs": {
      "sha256": "7f45cc827efce0aab01ea4cb0dc6e432ade6d0841d059b1a4623b99e3fc38653",
      "graph_file_node": true
    },
    "crates/conduit-http/src/gemini_handlers.rs": {
      "sha256": "c24f3dff7897ad2115c3fcf0e81a36f94750db08e314ebd9b87ae387e75d8d1b",
      "graph_file_node": true
    },
    "crates/conduit-http/src/graphql_handlers.rs": {
      "sha256": "a3c1a47e1b0a84223d1a8ef7e3d4701b89b480e6bd239903c86de8b51ed86c13",
      "graph_file_node": true
    },
    "crates/conduit-http/src/health.rs": {
      "sha256": "97519a7df39157d4273eab0b7107c882d4fcacadfe54e8cce25a692d36209f9b",
      "graph_file_node": true
    },
    "crates/conduit-http/src/jina_handlers.rs": {
      "sha256": "cc58f247372cff46fd8b35bb78944d2ab6fa806b370ac3bd332e23783f1101f3",
      "graph_file_node": true
    },
    "crates/conduit-http/src/lib.rs": {
      "sha256": "2da1a0373eea57fd106cded9e28199439d95617cb1da1a8e5ee897f7e4927762",
      "graph_file_node": true
    },
    "crates/conduit-http/src/middleware.rs": {
      "sha256": "49cdfdf8601f470628e506c23fddcb3d9305192e526b64381a28b927ae583e25",
      "graph_file_node": true
    },
    "crates/conduit-http/src/middleware/api_key_auth.rs": {
      "sha256": "775ae183beccb3c29e250ed1e68763ee44a1a0b717ad17cebc728409ffaf4503",
      "graph_file_node": true
    },
    "crates/conduit-http/src/middleware/error.rs": {
      "sha256": "a87030fa0e83d72045c2caa8e130713dbb94ce1d3299644638a82cf7b73448c2",
      "graph_file_node": true
    },
    "crates/conduit-http/src/middleware/jwt_auth.rs": {
      "sha256": "55ffc73d1e6886e5825408ae31f0b924acda6d17fc1495c2adb5d33dd97177af",
      "graph_file_node": true
    },
    "crates/conduit-http/src/middleware/metrics.rs": {
      "sha256": "d4faa4adf583880b69f8008de85b10a1e80b4b606ff494c499250ae659214560",
      "graph_file_node": true
    },
    "crates/conduit-http/src/middleware/panic_layer.rs": {
      "sha256": "673ab58efe8125b9fddce7a8b12964951a7171fbb782c46b61103fc535b8b77c",
      "graph_file_node": true
    },
    "crates/conduit-http/src/middleware/runtime.rs": {
      "sha256": "93222292ea681fc10fb508bea8db5485352ec1919c20cd9d3233e6493fc63d72",
      "graph_file_node": true
    },
    "crates/conduit-http/src/oauth_handlers.rs": {
      "sha256": "e34bb430220e37801d65b9b4aad11340628cddf3f01ed4e713fa9c69498b4233",
      "graph_file_node": true
    },
    "crates/conduit-http/src/oidc_handlers.rs": {
      "sha256": "4f1737c8f55010e030ed9a32dd258d8b356c32d7a4638dd06bd4b4400d7a1763",
      "graph_file_node": true
    },
    "crates/conduit-http/src/oidc_helpers.rs": {
      "sha256": "623ebb26cd61da596893ba269324fa2ff40fa7ca676020009929b605bf5f99d0",
      "graph_file_node": true
    },
    "crates/conduit-http/src/openai_handlers.rs": {
      "sha256": "543ff7445cab7802479bad33f92d57a1a6b4d27f7a1e03d1dca4996e90e87cb0",
      "graph_file_node": true
    },
    "crates/conduit-http/src/openapi_graphql_handlers.rs": {
      "sha256": "587431cf9a64224a1fb91cd85dc2c544ccae874b2d0d870fc77e7287f2ac184c",
      "graph_file_node": true
    },
    "crates/conduit-http/src/request_content_handlers.rs": {
      "sha256": "6adfffe2d09b81b11a7eb24b0f4e70c561e3eac6d62f145fc4cf519cdea73d5a",
      "graph_file_node": true
    },
    "crates/conduit-http/src/request_content_helpers.rs": {
      "sha256": "bd76b3be7a9708dda57eb7bc81e04f4681175dad2e00ff0e978f084608831065",
      "graph_file_node": true
    },
    "crates/conduit-http/src/request_preview_handlers.rs": {
      "sha256": "3aded8ec9e81ed8725e91db0142469a0b955c43317da33eda22306dc66913512",
      "graph_file_node": true
    },
    "crates/conduit-http/src/router.rs": {
      "sha256": "f903f44348aa85d1d94e045e4ebe85b4ed359452d4bc9ae98df8c1c6428435a5",
      "graph_file_node": true
    },
    "crates/conduit-http/src/static_files.rs": {
      "sha256": "f8c15294dc8fb2fb7fa9900dd453b2e7aea905d7e6b4bf316cd8c4d91b673e2d",
      "graph_file_node": true
    },
    "crates/conduit-http/src/system_handlers.rs": {
      "sha256": "d08ede04493f49606b7e30932b9d787de7ebae93be44acab8d8ebeb0238d2f68",
      "graph_file_node": true
    },
    "crates/conduit-http/src/webhook_handlers.rs": {
      "sha256": "2d64afbb7dbde7b717ddbb6157059cf5bda03b3ba4b951b3d45bb479567ed5e0",
      "graph_file_node": true
    },
    "crates/conduit-llm/Cargo.toml": {
      "sha256": "2c1426cbf8ab922f358ac201d7f0be0c4954cd77f634071f86592ef3d14e7a40",
      "graph_file_node": true
    },
    "crates/conduit-llm/src/constants.rs": {
      "sha256": "ad03f0252bbe6a5a288226bca40c68f619bc2a274f02388d1cce92e142fec240",
      "graph_file_node": true
    },
    "crates/conduit-llm/src/http/client.rs": {
      "sha256": "4004c5eb9ec80f01e500790db741b23b9f326691836dfe056d11d7e84ef1059a",
      "graph_file_node": true
    },
    "crates/conduit-llm/src/http/decoder.rs": {
      "sha256": "ff7e65bed51f464bd0e988ef0c13b8ce4b234c17f8e9ca70763b855bb0ce4d1a",
      "graph_file_node": true
    },
    "crates/conduit-llm/src/http/mod.rs": {
      "sha256": "fb23fa85ea942954143e22f60f2c20684cf5ab219fed34c4a88d3f178820972b",
      "graph_file_node": true
    },
    "crates/conduit-llm/src/lib.rs": {
      "sha256": "9d52c53f5c25d9d199732bc6dc73951d84a8ab12cfeb3ec05fd746a66cdaa3c7",
      "graph_file_node": true
    },
    "crates/conduit-llm/src/model.rs": {
      "sha256": "46e89ed2c2c378d3fce7ee5e440797e8190e9c5286d046f7e8868b0219266fa8",
      "graph_file_node": true
    },
    "crates/conduit-llm/src/usage.rs": {
      "sha256": "47f111060bc5f8396c690de29e758ef4a3fb4484886ef125f75f37a6d71cfb2a",
      "graph_file_node": true
    },
    "crates/conduit-llm/tests/http_client_integration.rs": {
      "sha256": "1181dc992f73ba916015c7e08760b5fcbaeec963a65979ff2299050c1b2c4e44",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/Cargo.toml": {
      "sha256": "8e7afb6a1082076cce9004f26e2d19a008f65a9ec1cad5a773c77e57ab56ceed",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/contract.rs": {
      "sha256": "0d4ed3a131bdf7b0851e6254ecd7862972ec4f086de5754a34b0d29f124f1b39",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/guard.rs": {
      "sha256": "b59d67fb95e3174aa1f676d92b6e5862630bfb7bd3eac27f4e98336a9c7b594e",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/lib.rs": {
      "sha256": "730d3e1f6083c4457e84cef788f8eb3b76377e9c4787d7147f1a185836aa6d78",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/memory.rs": {
      "sha256": "499c3e35c0306f5e7dbc4384d022aac197a05d78b9805cacca64d28e01311846",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/model.rs": {
      "sha256": "76fd9bf26d167013d12f15d614c2ce8d5e5822d24b3b7098723ee7b12889dca3",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/resolver.rs": {
      "sha256": "aefb48b838051dea49ceff760a9b60d8595a7bd337caf2b5eb57b64e5859df19",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/scalars.rs": {
      "sha256": "db8d41d685206b74511766edcad9336c172ea864980922b7796e76d5a6437a94",
      "graph_file_node": true
    },
    "crates/conduit-openapi-graphql/src/service.rs": {
      "sha256": "3cf8d4eafc2fb2788e6b4bd1af8de06926394b7055fdca30adca82646bd773ee",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/Cargo.toml": {
      "sha256": "9ca330a6444eab7abda947c6dd8c9b7ca54cc1ed11ae0b2f53c9057896b31e7d",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/bridge.rs": {
      "sha256": "096aa83915ed961fa0f29b688abab9448761e5d4d559da0155d27866b36b98f1",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/candidate.rs": {
      "sha256": "61b46f19e2d58223172ff74700acd9b3c2a07bc544db17cb70f965dff4b29c8e",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/candidates.rs": {
      "sha256": "f47e9890a8a6518f59056db706579cd2c8d49714f4346874dce48adc5dac8062",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/candidates/tests.rs": {
      "sha256": "275d2e41042aea9c23f457a5b7caf8e6776ab99dd1416810a5e1d65713c95116",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/channel_limiter.rs": {
      "sha256": "907ffa1a50b6afd98e70da4fe84b65b4ea60687517c1f9baeff276bdd0dfbac1",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/db_candidate_source.rs": {
      "sha256": "697fa0ce699a9785c73e3dd1323d3ed1f63725b76a2166bd2168a60dbbf01784",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/errors.rs": {
      "sha256": "d7eb2cc0971134105cc2106b1e5b6ec7052d06e93bd91c623dcf987451273bd6",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/lib.rs": {
      "sha256": "2e20b04e5222e338771756b3280db5f194ef4438f0da7c7432a0e9f0ba682872",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/live_streaming.rs": {
      "sha256": "21dd374a3018e900bbb0db0e5eef05d689cf1bebebc2583b9cde726b8727faba",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/load_balancer.rs": {
      "sha256": "e5fc004d1bac28c30333fe91ba4df5ca5be3520d036b9a30a73d0a1d94dd6d75",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/auto_reasoning.rs": {
      "sha256": "4be04e8596b039dd48ccb1fd5f8d834fd9663bf5773fd59ffaea7478999ee0d8",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/body_override.rs": {
      "sha256": "4ac02fcaa28cd4cb502d62c2b6ebc0159ab766ea9a5f6e62e9f92627c7a88318",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/capture_provider.rs": {
      "sha256": "11c520949a88f138ffead3355193e202a5a9f91404ff2df6f1e8e491707d4f0e",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/capture_stream.rs": {
      "sha256": "f4d572982d04f29739284724fd74c4dbfb2f91fb9d51954072476eee5d7ab8f1",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/channel_limiter.rs": {
      "sha256": "fc1f881f39904696f5387aebc4370122f281dadac77b6302370466824864cf19",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/circuit_breaker.rs": {
      "sha256": "4c013bf437cfd90e0e2836196b2d14c16e368682df83f9658af0f5de43f800df",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/ensure_usage.rs": {
      "sha256": "b30d00ed3ed5a315d3f559b35c8ad136682f9439d542925b078e62540fae851b",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/live_preview.rs": {
      "sha256": "ded23ebcc7f2377bfde9dafe8f40d7a2843609c0390e41b2fa229b599589ddab",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/mod.rs": {
      "sha256": "49d1cdc074fc5d405d4f71061e60cf7ef17acaf6261757314ed539708e02730d",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/model_access.rs": {
      "sha256": "8be1dfa936131687a089305953542eac3b5008f5093eb323cf8f41c42ab4067f",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/model_mapping.rs": {
      "sha256": "ea43a92b16c569f6287b4ff82617a2a904527ba3e55ef8444ab2e0a34f5742fb",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/override_request.rs": {
      "sha256": "5e4c7ac7675d25e3d705186c764e26ae8dd117dbe9fec35d6288945bc85877c4",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/pass_through.rs": {
      "sha256": "545e3720393ed989e21b2e2b036c9d0fe31fcdd78af13b9f19824ec640102f46",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/pass_through_body.rs": {
      "sha256": "0b03b8708e5f836fbae0095e2358176e92ea5ff1813b8bddcb6f42558ddbd4d5",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/pass_through_response.rs": {
      "sha256": "4c6ee72fcbf4925cfcfb9f98ee313a2d5b7813adef437d1e65336112436be2ee",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/pass_through_stream.rs": {
      "sha256": "2ca1e0e18a3a419419b34afaad06bb8c82fdb977e45749dd1a83319bd524ee57",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/performance.rs": {
      "sha256": "e4bbe56a61166024629666fb6234a8a6c7a7161077565bb218f44cd7ddb76754",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/persist.rs": {
      "sha256": "787e6f6f7f638fc3531cb61d7f2d10dc34cbaf02b3f4753aba849f010c7d976e",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/prompt_injection.rs": {
      "sha256": "f2c5f4b7671fbf58984d6511bc6a2dd108129cdbde5596132dc1886904ac9ca1",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/prompt_protection.rs": {
      "sha256": "e833fe2198d0c97b062da10c60924f0a1948eb376bbd2a58eaca9c3784d794c1",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/quota.rs": {
      "sha256": "be9d7fc0c4b3a2dbab6f04ab6e6073a463b87ea95241caa8f861840efb36b7e7",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/rate_limit.rs": {
      "sha256": "ddce67c672f92594e2a96b35c5d2e4228b4f7071ceff4a4286e5815319348a17",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/rate_limit_admission.rs": {
      "sha256": "e92a205bf27fbbc6a0569ae53c2812386533e34bb43264e17f5b20d557a78df6",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/strip_billing_header.rs": {
      "sha256": "7dc71289c13f0edf3f343cd4c5a10fb693d6c903b439853670297d3663fa9fcd",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/middlewares/user_agent.rs": {
      "sha256": "c618daa2452bb461c081df92773129faef6cd7c9d6f48c674e00daf8e31b52d8",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/openai_bridge.rs": {
      "sha256": "3ddf3701420f8272f640dd055ce7e413a5eb58d7eeff76f647b4ee19c277099f",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/orchestrator.rs": {
      "sha256": "dd58ee3868a8996d170786be3316050ada436fefb78c8372baa15db36304a95d",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/outbound_stream.rs": {
      "sha256": "a60e4170a1ee4fde59d43b8583263e3b20ff4cddaecc1ff25d4c02f4d9394ff0",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/pass_through.rs": {
      "sha256": "b3766bf205dbc9bdfd254763dfd792875ae6f1c9a213295847b95f466ae236bc",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/pre_execution.rs": {
      "sha256": "511e3ab7817c762885777968e6558c830e9cfd51b4ce8cafd9e25245115c1966",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/quota.rs": {
      "sha256": "99c0c68dd85e87ec448420fe5b4b1b1359bfa776d06b42c60d1ed94d21b005ea",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/rate_limit.rs": {
      "sha256": "b0f81acd40dd3f60ed76de13e3f139751eaeec66c4fad7fbf4b9a717a7bea128",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/request_recorder.rs": {
      "sha256": "a9979032c150c07b051d8c6bbff6b5bb39cbaf85ba1afcacbe7db8a16a4b5452",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/src/upstream_executor.rs": {
      "sha256": "deef74323b64c89ebab0a1809e0852183426edf1beebf01893dd89bdd134e09b",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/tests/openai_models_golden.rs": {
      "sha256": "36ac0b5782d4e0b50e769d2c93301500f857e123b7b232441f3739175e3ea593",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/tests/orchestrator_e2e.rs": {
      "sha256": "c24ce760099de5adf1cbd5475f870c5d304ce63a0cd5da2f37b2de06f92ea9d8",
      "graph_file_node": true
    },
    "crates/conduit-orchestrator/tests/wire06_outbound_smoke.rs": {
      "sha256": "16251e757796128c6fbf75782fd01deaf917a7a7b50d08b643d2ef647ed4911f",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/Cargo.toml": {
      "sha256": "ef76aabad3b17bf35fb6ef1c6532ccdad52c2271acc84cd388e6836eec2cfe87",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/cancel.rs": {
      "sha256": "5c2be6ce6a65ee978006d27be3cc0c2e5bb540935563a95b0737ab72bdef2650",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/empty_response.rs": {
      "sha256": "0e669e42741cd1863b6ec09f41452ab6feb52473ea41753e7c687fdc364a93bd",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/error_rewrite.rs": {
      "sha256": "fd8ac1cc026dfc6c5f1dc601cd7c2c877a74d0cea7637040d5ddd339c71c4e48",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/lib.rs": {
      "sha256": "ddcf802821c282e5c1686d5490504b0cab099e95b1db8afec6a7d22fd8239c33",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/middleware.rs": {
      "sha256": "71e2ec2060bf5447e6b2d5fc1e17b4f5f4b7392f17c190db08ba4cdeccf59c3f",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/pass_through.rs": {
      "sha256": "ba5f910e378e958f463a11f40929058b99a5ff7fa342091b8a2ea03b9c5a2f4e",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/pipeline.rs": {
      "sha256": "9b90727f95d7285c8da295d8b638b005bb8ce0abf43ca365a0f6385b9ea806f4",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/retryable.rs": {
      "sha256": "114e2ab2a70604b0c591a9171a4e5aa5a5e75c1086c84d42783d63d342781270",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/src/upstream_error.rs": {
      "sha256": "f91446531433ca3f47cffbd14f27b8e4ce56e18463ae58b0eb4295159eb778ab",
      "graph_file_node": true
    },
    "crates/conduit-pipeline/tests/protocol_routing_matrix.rs": {
      "sha256": "4bc7cb700122f2527a8539b09c62b68b430c715ebbf230f53aa1bb4ce276562f",
      "graph_file_node": true
    },
    "crates/conduit-scheduler/Cargo.toml": {
      "sha256": "dd865ae06861c81d4b2d02840731f3e88858d4b6e22dd0354d604ba494537a78",
      "graph_file_node": true
    },
    "crates/conduit-scheduler/src/jobs.rs": {
      "sha256": "8157463e6211c498f04dd8622d9a852cb96b7c3f778546ca35ab9f46a603595a",
      "graph_file_node": true
    },
    "crates/conduit-scheduler/src/lib.rs": {
      "sha256": "7bb704ce726b93d417b682d0aac5836e64f95d9d6cdcbde89fb951f20d4f82e7",
      "graph_file_node": true
    },
    "crates/conduit-scheduler/src/runtime.rs": {
      "sha256": "8b7565810350b4d7af47fc6de4ea2eb0aa2c7da60fd732dc928f1ddf67074807",
      "graph_file_node": true
    },
    "crates/conduit-scheduler/src/tasks.rs": {
      "sha256": "cf9c062d2ffc28c7128cce2c2c881ac0075b7ef6c39322267b7757267d3b3f3a",
      "graph_file_node": false
    },
    "crates/conduit-scheduler/src/worker.rs": {
      "sha256": "88b55dfd73a7ac0169a1519ab091cb9e8beea93772bd6ac2296bfa3e95d62db0",
      "graph_file_node": true
    },
    "crates/conduit-scheduler/src/worker_logic.rs": {
      "sha256": "40f5ab8471a161b4d348aa746aff071248f3371295288d31cecc1b0f4b0fa686",
      "graph_file_node": true
    },
    "crates/conduit-services/Cargo.toml": {
      "sha256": "10d6dbc3762af967a8bc0d91dd5a09a68f43775a03ec69e99e61dd8c416d74f2",
      "graph_file_node": true
    },
    "crates/conduit-services/src/admin_settings_service.rs": {
      "sha256": "c4a48202ec4fe9062653558ba6dc0e8e178992584af8a01f8d085a51b68cab63",
      "graph_file_node": true
    },
    "crates/conduit-services/src/apikey_service.rs": {
      "sha256": "f1074041597e450f3f3ba9e18b515d900e61c3b9001d8cc137a2ba0230cbedd2",
      "graph_file_node": true
    },
    "crates/conduit-services/src/auth_service.rs": {
      "sha256": "9fac7e907a6a50b4113ceddadfc3165e9de62ee03c1183af6f947434c7401b91",
      "graph_file_node": true
    },
    "crates/conduit-services/src/backup_archive_crypto.rs": {
      "sha256": "f71166a0a6150542ff796a4e09b2a47f5de181cf1fe18b289f627055239a8b75",
      "graph_file_node": true
    },
    "crates/conduit-services/src/backup_service.rs": {
      "sha256": "a739dfc62ef29801a77756ae2c79cb5a7e3972978dfabbb0fe5031d5ac99bc51",
      "graph_file_node": true
    },
    "crates/conduit-services/src/billing.rs": {
      "sha256": "d67bbbb52de0573e7ef6b8315f0acbb7d6442b349d67e9fc61d84b39a755198c",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/auto_disable.rs": {
      "sha256": "21ae095721a3947068db5aae027b0de3b839af6fe40507374f2a653ca6064163",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/build.rs": {
      "sha256": "ab9488aaf1c03a1b2e32cfdc1b297b85d426359c7f6a0a973ed7bde34ae97924",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/bulk.rs": {
      "sha256": "dc039fa97b96b165a40f90771b15f11f3bcfd6f76ce92f0dfe4276b1dd40b71a",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/credentials.rs": {
      "sha256": "0878103200d701f06f7cbbf652b177452f7fd612d7b4b11d0d574d3af505e499",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/endpoint.rs": {
      "sha256": "2012388f2eb6e413dff9375e838a2d236332642f02d9dcd2369074558111eea4",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/list_models.rs": {
      "sha256": "aa4b1321ca46846d640145e00c1c4de1c547bb507095024e37aac5e40ac9e9de",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/mod.rs": {
      "sha256": "76fd780d9b2da3f380b4db84efd04961368c276040eeaa70421cb8cfda869ed8",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/model_sync.rs": {
      "sha256": "ec2fdcdfc4ed801ca0c1516cc65ed6578e4ebc509d4b60704148b707df782f0e",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/probe.rs": {
      "sha256": "d405c94cab05d9188489ebdd55dafb35b9a977903ae176d658aeccec5b627862",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/rate_limit.rs": {
      "sha256": "b44ba6f16d6d5509981b1e6a9ccc7afaaabc15bb970bede4d4a7ce5902e11d34",
      "graph_file_node": true
    },
    "crates/conduit-services/src/channel_service/settings_merge.rs": {
      "sha256": "16a35108ebf843fa974905fe91e5e007fa12a5d89cda776f92b266b4483c062e",
      "graph_file_node": true
    },
    "crates/conduit-services/src/file_service.rs": {
      "sha256": "1510d0e87deddfa921125f97285bfaf07bfc9124102b80ade070a09860997e4a",
      "graph_file_node": true
    },
    "crates/conduit-services/src/gc_service.rs": {
      "sha256": "14dd2fdb947befd96a4f031f0b62ee197ca56530440c0ab7fe9b32df9c11e3a5",
      "graph_file_node": true
    },
    "crates/conduit-services/src/lib.rs": {
      "sha256": "f4adfb9ab0ca7ce87c656f651f9543070c22dd89b4973c0e5b0fdce509844422",
      "graph_file_node": true
    },
    "crates/conduit-services/src/model_price_service.rs": {
      "sha256": "420f9410fb5c8bab56473ca2fd7bbd32cec14912251d2cbeee737a2b0f6ab894",
      "graph_file_node": true
    },
    "crates/conduit-services/src/model_service.rs": {
      "sha256": "7668d198e16f49e887d7198ffb2812fe4d927626facb305b493026fb704ee3fd",
      "graph_file_node": true
    },
    "crates/conduit-services/src/oidc_service.rs": {
      "sha256": "f3455a7491ec9dd34d84bcb207293408b0047f25839e9f96a295039ed6315cd6",
      "graph_file_node": true
    },
    "crates/conduit-services/src/project_commercialization.rs": {
      "sha256": "239d2410dfb806ed23cb64aaadb79e580ae6d41860fe69a4555dfa1028586051",
      "graph_file_node": true
    },
    "crates/conduit-services/src/project_service.rs": {
      "sha256": "4bc0b44fd0eb5719699965ab01c63db56ec86536d6093e9e75f96245b5544559",
      "graph_file_node": true
    },
    "crates/conduit-services/src/prompt_protection_service.rs": {
      "sha256": "fda22c0759a1d3db5b9caf9d74457cf7eeff130239676c487bff8f502f17e378",
      "graph_file_node": true
    },
    "crates/conduit-services/src/prompt_service.rs": {
      "sha256": "44a773689a37c48fc8459478bcb4d80e384b1bbe363f39e92a441e199914fd0f",
      "graph_file_node": true
    },
    "crates/conduit-services/src/provider_quota_service.rs": {
      "sha256": "6f0f7f4f7533441b7518dd5bc0647245a9c40e1decabc15f6e0ae61db0fa50da",
      "graph_file_node": true
    },
    "crates/conduit-services/src/quota_service.rs": {
      "sha256": "c838b8d57aacc3f3de5e63916e9f52f3df68e4f78db6f85941ab61bb1858cde7",
      "graph_file_node": true
    },
    "crates/conduit-services/src/request_service.rs": {
      "sha256": "88fc2d52cf04a71abcb5a5bc2b42653eac716cc0953dad311943d38311ca61fc",
      "graph_file_node": true
    },
    "crates/conduit-services/src/role_service.rs": {
      "sha256": "f928f6268724101a5c36f50218783c32723836c04e904a21b6a1a14c62d2afba",
      "graph_file_node": true
    },
    "crates/conduit-services/src/route_health.rs": {
      "sha256": "cd485c73576cdff53115d88104fdba2f6cf96c4ddd1fe4bb1c08f432fd8eb802",
      "graph_file_node": true
    },
    "crates/conduit-services/src/system_service.rs": {
      "sha256": "b0d5e0426fe35dd07e7f28d541493af93d8c084da78147ccca5ef14de24e65e0",
      "graph_file_node": true
    },
    "crates/conduit-services/src/thread_service.rs": {
      "sha256": "fdd6e302b8b6efdcddfeca50789757b7440621af94a956da35e344cf6f5ef1a9",
      "graph_file_node": true
    },
    "crates/conduit-services/src/trace_service.rs": {
      "sha256": "da73dfe6b20e4fce2bca59e8e1c4ec04fb1f7a39296fe5880e659f9c4c0e5e7d",
      "graph_file_node": true
    },
    "crates/conduit-services/src/usage_service.rs": {
      "sha256": "f83affc8368cd46fea289bba28def9c043c7778a094c8a174f27244a690b5c7c",
      "graph_file_node": true
    },
    "crates/conduit-services/src/user_project_service.rs": {
      "sha256": "46b1297f8652cee3dd27162c364ea2cd8b0c0031059a5a7ae3d724177083820f",
      "graph_file_node": true
    },
    "crates/conduit-services/src/user_service.rs": {
      "sha256": "6a896c2c7c6ddeea20ccc87a2800c255d9b047e3ad2b28b94c48dae70c1686e6",
      "graph_file_node": true
    },
    "crates/conduit-services/src/video_service.rs": {
      "sha256": "ba3b885abf2ef359a3b05c89e38859287f2f930f356dbf300c230a9288992731",
      "graph_file_node": true
    },
    "crates/conduit-storage/Cargo.toml": {
      "sha256": "2cee21a9c631b2e0edfbe50473d341a10b4056b873174ae1966d4bb998ad33fd",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/adapter.rs": {
      "sha256": "b09bfd00049b23e14a376a7f31ab07d55f0a1af01aecf924b555dde84789f3ad",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/backend.rs": {
      "sha256": "ab60646ee044071d437bcc59b321ab4d60b1fc8167b05ea10d89742bc1449a8c",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/gcs.rs": {
      "sha256": "bfb0e2bdb66d8a0bbef7a673f535c9055efcba94a2dc3f2ea6c28d7ab84ec6e1",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/http.rs": {
      "sha256": "f7bf81b6b2a481569d37dc86e3ebbae53d3afdd53e95c9305967f351a4fdfa5d",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/lib.rs": {
      "sha256": "25ea2d6f30c763729ba19b59ebef529872fc7dfa643e4f5491031e993e9c7861",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/s3.rs": {
      "sha256": "d7c119ee298625945379e978e969a190d82b83bf4063e5deee1df2150f20b5a1",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/service.rs": {
      "sha256": "e7ec4a943ea1aa3841b6de97ff88da4ebae6ad4f3b563148cfeb60d44c60bc7e",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/settings.rs": {
      "sha256": "58ccca96528ae18c75d70b1ad8f9dc32a75a37a743c6ffcb91852023121a298e",
      "graph_file_node": true
    },
    "crates/conduit-storage/src/webdav.rs": {
      "sha256": "2f420f3d697637cda5ed3be40c2d4fbea2270784e2a4e9ee3302eb9693588a5e",
      "graph_file_node": true
    },
    "crates/conduit-storage/tests/adapters_integration.rs": {
      "sha256": "5c18946106089cc72fa8f3d344891c5a939181d32eb02d1be9d10f5da552dbf3",
      "graph_file_node": true
    },
    "crates/conduit-testkit/Cargo.toml": {
      "sha256": "b3764c1b813e9d860bd0f35a1cb31b537bb58ad201bed288e5610de16c59a1f9",
      "graph_file_node": true
    },
    "crates/conduit-testkit/examples/new_api_gateway.rs": {
      "sha256": "5f7ef905a671fa0e079794928392eefd9b567042d027a5f28439dafcf4ebaf25",
      "graph_file_node": true
    },
    "crates/conduit-testkit/src/db.rs": {
      "sha256": "f22bf4635ab55f1963e5adf0a10cca6aa623483963da751cf77dd59b93c73b36",
      "graph_file_node": true
    },
    "crates/conduit-testkit/src/fake_provider.rs": {
      "sha256": "2cad361c5a2800a8251898af04729ad152c52d7013f72750a79e7ae095abb661",
      "graph_file_node": true
    },
    "crates/conduit-testkit/src/fixtures.rs": {
      "sha256": "1c487d53e38bbd67677047e9c68303bd5f55690c8a2b756f09a0d6bf5e5e8bc8",
      "graph_file_node": true
    },
    "crates/conduit-testkit/src/golden.rs": {
      "sha256": "c2932ccad9a9008a0e76167376a810b940404e47e51be17e9e44c26039a8ba99",
      "graph_file_node": true
    },
    "crates/conduit-testkit/src/lib.rs": {
      "sha256": "b4ebcdd7d414d9a1e099d8ad27a20fd52574c7ed5e0f6768b532a24144ace6bf",
      "graph_file_node": true
    },
    "crates/conduit-testkit/src/new_api_gateway.rs": {
      "sha256": "c7f69e2619bcf547b3579c3cefbbbfb71211b8af4139ee408780a897deaab24d",
      "graph_file_node": true
    },
    "crates/conduit-testkit/src/stream.rs": {
      "sha256": "ddaa1f481b89881526d4a9578c6d6a7e10c4f0e016d856e491cb1bb91385693c",
      "graph_file_node": true
    },
    "crates/conduit-testkit/tests/corpus_tests.rs": {
      "sha256": "35c9e2e3ba0ad4cc8abe920641f913d1a0051a209fe2039bda64afccc15a943b",
      "graph_file_node": true
    },
    "crates/conduit-transformers/Cargo.toml": {
      "sha256": "7ec88765dfe1040fa5f315fc3b12aa25da564d966344cedb2874352aecfaea91",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/aisdk.rs": {
      "sha256": "babf4d892da068b571687a1a99e0b5c6f0c54b84f5b9ec1e72655372b2e2a6e2",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/anthropic.rs": {
      "sha256": "f5db5b73c7a535d7e0ec70958612b3f81b2bdb2ce22e54643c361c30a51bb3a6",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/antigravity.rs": {
      "sha256": "380453aeba44389343a823cf04b46049e077b3343276f2b10aeabb84128399a9",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/bailian.rs": {
      "sha256": "f72804b05349999664ba0e7f182077ca74d2be2da7170cbe91e964849d67c3e2",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/cerebras.rs": {
      "sha256": "2e69cab406d47179caa35a9556175fcdfcf54c0d923523e88f36fbb92cdcf2a6",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/claudecode.rs": {
      "sha256": "d342da44b18da9e92b4750c8d536d99fd6b37dc53949a80d54ec3d301333a2cb",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/codex.rs": {
      "sha256": "98fd086895a47482e379f073369d01bf033d6d47d05be30173309a8f34f62f1d",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/copilot.rs": {
      "sha256": "ddf31c594cf1104edf094a3114d346f1563d52befdab641f2b4a55d5d3c574f1",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/deepseek.rs": {
      "sha256": "474ec84cbb90802f8bc5c534fee5c98b4b55eb219aef8ddf3bec5f458ede83b1",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/doubao.rs": {
      "sha256": "bb8a54abebfecc4cf5ccb4603559f3933c4d258fdc4fd58c0cb8c5eaeeca4f7b",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/doubao_transformer.rs": {
      "sha256": "810cc291617cbe2551ea2a9f303573efc6b9a796b0058f396b00b3a44ca9c45f",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/errors.rs": {
      "sha256": "b3d17241e7a501102b0db1dcbee0b99087abc6bfd3e1221dbe0f8d5a4a6f8910",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/fireworks.rs": {
      "sha256": "fe8b8206301eb8674984dea7de60ce9d41a4a8c15ce2dcb72043efcfa198b54e",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/gemini.rs": {
      "sha256": "45916074cbdb2fe63f8e483a26b39ce1f35d8277e865110d37580c481ae648e4",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/jina.rs": {
      "sha256": "2fa131d95e87f242204c43987c33a0faa1e3f796a8158ec99c750245fdebd081",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/jina_transformer.rs": {
      "sha256": "824b5433df5625438806b11890c8462a2b57354a7462a04ef915d9bef53fb265",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/lib.rs": {
      "sha256": "da570f60ae9a62b281ac401f26cb3fb51665a9a886f8ae21864eabb69a7166a7",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/longcat.rs": {
      "sha256": "ce92526ab8bee637e5f4fe9f86c48847baa5480459592d94e9f35eb0a55a02e8",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/modelscope.rs": {
      "sha256": "f1edf634290667bfd9c8f98596fd4218e983ddbce2676000ef974990e1689195",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/moonshot.rs": {
      "sha256": "77f29641037fee1e788d1320f3556f51b775ea8175cb5350b16faa52b698e9b7",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/nanogpt.rs": {
      "sha256": "a92e3714c0e8da083fd04476075b7756678e154955883992f92555f41d49ee85",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openai.rs": {
      "sha256": "363927848aff306fec37c9560f4c44787da130715f4b1e8fa229660db267f635",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openai_compatible.rs": {
      "sha256": "d3473ab30f14c08d9c482027df8e2b91d01b809ff0fdb6ec22920a8903ecb8c1",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openai_legacy.rs": {
      "sha256": "8217f5c7ead9dc9fba3ec850b39837e487054c89c59f8a585eaca6e26d97f31e",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openai_outbound.rs": {
      "sha256": "39cea0220ebb34dca4b5449741f2f50c3d0958c3cc6955295b3a8a7c293d647a",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openai_responses_inbound.rs": {
      "sha256": "6432e0ff6b99f1d38d94bd57d94823b64801bd694a3f00dcf5b5325c12a914cb",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openai_responses_outbound.rs": {
      "sha256": "e83f04e5177dc1112ddfb36fd614b9380c95fa360407a3321541f77c29ca8b4e",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openai_stream.rs": {
      "sha256": "d51bca6945c2ccabb76b50346123848a4613de492fa86ceddce1ee12705138e8",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/openrouter.rs": {
      "sha256": "3ef5a92805f9bbca9376913f4cd4c28915b5c49b57de9943d35971697d8e660b",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/registry.rs": {
      "sha256": "774bf08744b2895d6c5ec5f5af89292cef6481df75e2d45f410d0ca6143f7e23",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/traits.rs": {
      "sha256": "ae3d2932ffd4919cee7c6cc39e2c0ac3a1baa12fa8d388c58bdb9d24c45afcef",
      "graph_file_node": true
    },
    "crates/conduit-transformers/src/zai.rs": {
      "sha256": "e0ee6cb0f414131e2ffb9b8b830486b35f77fdef63c0095e9bc51b458250405d",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-error.stream.jsonl": {
      "sha256": "c80d57ec32a3cade7a06a672dc8bb4f5793f50379f6406319f74f2f377a81763",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-multimodal-inbound.request.json": {
      "sha256": "e66c09e72b5485041ea47ec55dad19b0307a355182cd5b7de826b61ff576d920",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-parallel2_multiple_tool.request.json": {
      "sha256": "74b12f33b2497cdca3f9998c9b44aced9774f424adfea8813cd3a050a7add5ec",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-parallel_multiple_tool.request.json": {
      "sha256": "03e22f607157cb888552b49e14bf1effe295b2725a110edc490caedbc6231653",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-simple-inbound.request.json": {
      "sha256": "c214357881e49db28b16e047ef94114b69023872d9f664b9745d5d9c3f74776a",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-stop.response.json": {
      "sha256": "1645b20ba4bfc841c9740cecc5c63a60f82be05b6686d98c63f8d9f11c83b9fe",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-stop.stream.jsonl": {
      "sha256": "2c569fb606ef77f8fb507d240a46d0331b37a3c38882eccfd2f796647da90f6e",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-think.response.json": {
      "sha256": "9082cf88c7f46c9df72b922347114f2897cbe932fa78ffff2181d3f22ee6d9b8",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-think.stream.jsonl": {
      "sha256": "868620331bc3bab34f35b66ec6fcb5110a46ec2a3e20f4851cc78986c3973d4e",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool-inbound.request.json": {
      "sha256": "adcc83ebb3bd16ba39dc661bf23d7759ed8c75686fa55c4fb640e72ad11c44db",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool.request.json": {
      "sha256": "57ad0ca52ab1a3d725cf6e32c984f83dbfbc1d958a0893515f9cc5f49a6983a0",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool.response.json": {
      "sha256": "e284fabe35addd6508e329678f3bc8cce604ad43f57a1f731739c541724b7252",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool.stream.jsonl": {
      "sha256": "fa2f72cd4a2538e3ae86820771f1aa12d9d6820da983dc4f36b9bbe69614558a",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/llm-parallel2_multiple_tool.request.json": {
      "sha256": "6efb512eba36ce5a3dac4026cde02b5dc5fa331a43093350be3fa23ce898758c",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/llm-parallel_multiple_tool.request.json": {
      "sha256": "2a54718b7eec26b557c2ba897a7d91a892567656b41a4c880e471d9b9b7ecb0b",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/llm-stop.stream.jsonl": {
      "sha256": "dc37559a3484cb0b9c38496a2aafafa28f28640369dab9d9959aa5908fefeb97",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/llm-think.stream.jsonl": {
      "sha256": "47780d6a252fefd00eee8dff4aab0b3959bc1533acb168957c0a31ea5682e1a7",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/llm-tool.request.json": {
      "sha256": "e9f6911656fb978b7b0581c2d1d56ea4292077ad4ce4d92242d40a6c6e710ae6",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/anthropic/llm-tool.stream.jsonl": {
      "sha256": "c6b69ee77cde72309363fb776d80c2290ea7a533a91e29908b4277ccd923320b",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/deepseek-reasoning.response.json": {
      "sha256": "39a435b2ec8914fe94f0d3993418de2f90b530d7e2481682fde189a336d238bc",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/deepseek-reasoninig.stream.jsonl": {
      "sha256": "6ed5fe33f89a0682156cfdfa789a8efad39ea648799dadea74b80f1a238b51c9",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool.response.json": {
      "sha256": "ea234eb30f7cb90093a58754e0a2443ec72c43672d13690c9469145f94782eda",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool.stream.jsonl": {
      "sha256": "3e0063ac4cd59ce254234bac603ff2b83d59abd72f33ecf76f0dc797d07ec0ff",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_2.response.json": {
      "sha256": "76318c36d65f71c8ab37ea52b75b8795f4253e537208299692a6af882a9c73b5",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_2.stream.jsonl": {
      "sha256": "93ab45f8ebd4b06be7171560bb61e58f84b45c562c8651933a9a3e8da1b1925c",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_3.response.json": {
      "sha256": "08b1341bd38d0e72bfc55dd620312cd47b476939f475ba6c06dce4a23480fe48",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_3.stream.jsonl": {
      "sha256": "428ee3ed26916b7e6d14c1e14a10d97380012e359c780475817e2a658413f4cc",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-parallel_multiple_tool.response.json": {
      "sha256": "0cbc113a55b4ff6d418831ca74478ae90d21564661afd77b05f280efbee8ffc2",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-parallel_multiple_tool.stream.jsonl": {
      "sha256": "0663ffac2314affe9b0e0842a5805696aa274633875fae019d94819451e765a2",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-stop.response.json": {
      "sha256": "774a46b0ae38a3208e0bd45aa791c0a0956c67e77c1c0470c900c12ec0ff74cc",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-stop.stream.jsonl": {
      "sha256": "968978c274b4f20bf9f35937dd2febae313f31557d15e9f763e23510e844449e",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-tool.response.json": {
      "sha256": "89c99d181276f821bbc4491dbce4cf61e10b1f7cfdc94a3db5100fb9e94a2503",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-tool.stream.jsonl": {
      "sha256": "5a10e5594994f7d7efb0a77f16acda163d611e6ce66b849e4aa9b7bbae8e81d1",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-tool_2.response.json": {
      "sha256": "1876f563d7e57cb9c1f94a380d7806b14274731db232fa2c34d3fc82324c146b",
      "graph_file_node": true
    },
    "crates/conduit-transformers/tests/fixtures/openai/openai-tool_2.stream.jsonl": {
      "sha256": "bb669d7440423ccd59c00212572253f519baf5f25727b297431e9f53b80a731c",
      "graph_file_node": false
    },
    "crates/conduit-transformers/tests/protocol_conversion_matrix.rs": {
      "sha256": "43897ffbdeb2c0a6d99345c6d1856c134acfcbccbe7a8637fc337a2c19a37bf2",
      "graph_file_node": true
    },
    "frontend/.env.example": {
      "sha256": "f38d7f309ad1f1c20868f7c08ac56bb80517b9de90fa2d01751743fa0cccf942",
      "graph_file_node": false
    },
    "frontend/.gitignore": {
      "sha256": "d86d7d485c745dde7fc54a531b86af7bf3190edc268c5bdc118046385ab35cbc",
      "graph_file_node": false
    },
    "frontend/.npmrc": {
      "sha256": "5f947ee7a93b78e2d3131807d3f9b355b5bb237b9a88b6909113f4a3fb6de740",
      "graph_file_node": false
    },
    "frontend/.prettierignore": {
      "sha256": "4ef7ef6f62011a3eff5acf08ef42586c5f6304e4541855927d3fee9f8e52c106",
      "graph_file_node": false
    },
    "frontend/.prettierrc": {
      "sha256": "235de18f425845a7d8990e4765b8cd0e18367f0956e3f2e155f2aa5e284e9fc1",
      "graph_file_node": false
    },
    "frontend/NOTICE": {
      "sha256": "0f5c1cb570dc913ca2218f38085cfbec59169b0382598abccbcb7a82fd213858",
      "graph_file_node": false
    },
    "frontend/components.json": {
      "sha256": "275fe02d1a90e3a10bdb5af01395661015c2bb85d569e9bd52dfa7cab77bcaa0",
      "graph_file_node": true
    },
    "frontend/cz.yaml": {
      "sha256": "caea634dad425d65ea035d532c7457998a79b228480690e373dffe36c0d8faa3",
      "graph_file_node": true
    },
    "frontend/eslint.config.js": {
      "sha256": "bec25b66f86caa2022d0bc85932c60459fa9aede69540dcbfe734c4d80e9b4b7",
      "graph_file_node": true
    },
    "frontend/index.html": {
      "sha256": "bf94868b2973c74ec23dae46c832a4cfb9bdaddc1a363c1081a0b614830f32ea",
      "graph_file_node": true
    },
    "frontend/knip.config.ts": {
      "sha256": "d4b854916a56973e75f2dfffb5749c62901b4081b3ddae7ebb3614ac7d0fca49",
      "graph_file_node": true
    },
    "frontend/package.json": {
      "sha256": "23c74eebc5f177604f864850740b83a978007827969804e99d12c1300fbe0b23",
      "graph_file_node": false
    },
    "frontend/playwright.config.ts": {
      "sha256": "f13234bd879f4936096d46f3a9d85f38a19bd5788c12477c9548e07b4e65f217",
      "graph_file_node": true
    },
    "frontend/pnpm-lock.yaml": {
      "sha256": "bcfbd122f71de60ed1375988965834ea77f8669ce0f5b05322a99b4e494e7a72",
      "graph_file_node": true
    },
    "frontend/pnpm-workspace.yaml": {
      "sha256": "63725c64de94c1251d91b7ec1234e218eac2e52c39b3d81a2593b5e519616205",
      "graph_file_node": true
    },
    "frontend/public/favicon.ico": {
      "sha256": "d94482f7a3488a593a93a3ca37ea0b0a9d78a1ed83bcbd8e21037f855df7f978",
      "graph_file_node": false
    },
    "frontend/public/logo.svg": {
      "sha256": "8d7384ba8b97018194d06668fc14bbb10b96f3c077cf4e233d3de8dac20e815c",
      "graph_file_node": false
    },
    "frontend/src/assets/vite.svg": {
      "sha256": "63a264571d3a85cde99dceee29618929ed5a930145a64f86844ddb7e6b831127",
      "graph_file_node": false
    },
    "frontend/src/authenticated-layout.tsx": {
      "sha256": "3050df6b23256fd004b38fc3392d760e80b9874c4f2b35ef1764eec6542ccf10",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/actions.tsx": {
      "sha256": "507bb95d0b69f0fb5fa7cf1599051b652f9d4f462cfef8ad66a39eb780af7fba",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/artifact.tsx": {
      "sha256": "d922c1f6b4bc16f58e8d212a8acf90a1956b35df204c4edb9925790645e32bd3",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/branch.tsx": {
      "sha256": "3be1dc1a4e805c5821086697b0c0f97819b74f3514ea08df9b0ff2f6f1f1d3ad",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/canvas.tsx": {
      "sha256": "25ce4c3bd5e882139f0aa91e02e03a9cf62dbfaa12b856d1aaa40b267aa1d5db",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/chain-of-thought.tsx": {
      "sha256": "954e0120fb19a8bcd1a6a4775fbd8d87a0f884b4995b0466fb8c1593316511d0",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/checkpoint.tsx": {
      "sha256": "dc5a7c772eecaa6077629d759466cfba8b9740e0936f407a9a0d45f5ce74f4df",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/code-block.tsx": {
      "sha256": "503a26da7f144a05be9a82b81d564367c23836a843310101dd6dcaa32bdfa719",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/confirmation.tsx": {
      "sha256": "2c1f8b8caafb9e7d34d7351eb4210b8be42e9708a674ea022b6f05a78b8dd850",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/connection.tsx": {
      "sha256": "f988c1a5fa86de7bc60e098267af48303826608348f7ab72ab0204052377b214",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/controls.tsx": {
      "sha256": "b2e6792183bb966eb388c159c50138394f23b13890330a8c3c45de5ad3405c58",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/conversation.tsx": {
      "sha256": "5552930b28af3ec8394dd39a1e6ef7e8538460b37405a01e0d65b88987bca4ad",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/edge.tsx": {
      "sha256": "29e81233e94a90fcffe52c1f7021672b1ad06ee2603acb197485423544a487ab",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/highlight-lifecycle.test.mjs": {
      "sha256": "67f410fc22c694777c4c24967929c7085de82939d3c3a5eccb18f5238e479f6b",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/highlight-lifecycle.ts": {
      "sha256": "ad30f9d2b9012f55725d9135b32b6d775d1fe38454d81485b5c7976761c883c4",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/image.tsx": {
      "sha256": "d83972ee72b4b31405a52b48c9126a6e340acfc07b43a3ab36ec59ab80ed7132",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/loader.tsx": {
      "sha256": "108dafab8160a39d6476d47804779f1011269624edc3ac1745139c76ecb643c4",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/masked-code-block.tsx": {
      "sha256": "137aea61bee790df57cfd8aeefcb08e0103e3ecf97bde2c1362b591dc0ecfe4c",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/message.tsx": {
      "sha256": "3a40627f1751879928a981ff231ea4fc0fcabc5e319dbb82282b107b0cfc17f6",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/model-selector.tsx": {
      "sha256": "a408eead2b54f70453f79f383e4243d7c6af8acb8331b59722d14c36e7a5a6c9",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/node.tsx": {
      "sha256": "cd813443c5464a1f8aa99f17236f4d34c2b9b9459ad9f109fba529b3671c3fbf",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/open-in-chat.tsx": {
      "sha256": "99437c7e16f280ac35f94d394fd5efe5236691da55ac80188a38baccd13f6a0e",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/panel.tsx": {
      "sha256": "5519e94cf4c67cea56dd3cbf56adfe40f3f4da5e005e5494b7d154a69b54c3fb",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/plan.tsx": {
      "sha256": "e18ce5566b564b2e731a8a89e29e13100390bc1a31690801eddc052b343e533f",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/prompt-input.tsx": {
      "sha256": "910aa7e8e81a7c703573db16cbc54d9b5132fe1b79f45a03de3459bd431e446e",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/queue.tsx": {
      "sha256": "54131d51f0a85416f96c783fbf5901feb9cc3fbfdc86bf865301dbd20ee15134",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/reasoning.tsx": {
      "sha256": "7a43d3261be02a688215f0d12d20cf41dd29393e958972949429eed9949c1692",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/response.tsx": {
      "sha256": "861966fbc08923ffba0d5619daa5af4b1d154b058cf83e1443e8dd67d7258540",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/shimmer.tsx": {
      "sha256": "41fd42653c23dd61f431201e9915330628375ae42e9fbae38f0f7d562753a22a",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/sources.tsx": {
      "sha256": "7bc5a944dc5cda8b6a781fba9f3243eef4a4e0ae083382d864f738072de66402",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/suggestion.tsx": {
      "sha256": "d60e2e186d162dd5eaf957a097af6cc773365716f94219a8a53c2b267a0e0388",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/task.tsx": {
      "sha256": "c26f2d627f50b7c054c78d78d8e63a5e193ded6633c5f0b2c40b1f200527ee6f",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/tool.tsx": {
      "sha256": "98ee3c7026b5f45cdb9fd4dfb7b7d93be2e966920e4783249b0bf16f37f7920a",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/toolbar.tsx": {
      "sha256": "ef2b45749df4b09a07f0500294fa3ac20a3e32ba65c46ef1244bf69fd368060b",
      "graph_file_node": true
    },
    "frontend/src/components/ai-elements/web-preview.tsx": {
      "sha256": "a54fb9f1f1c8637b7c0c000f7aa32647cb1144eb9fbc846eff818c924ee6c9bd",
      "graph_file_node": true
    },
    "frontend/src/components/auth-guard.tsx": {
      "sha256": "4046c246645f79faf8449f4bc90a0134639c819151b6460bc624394420849947",
      "graph_file_node": true
    },
    "frontend/src/components/auto-complete-select.tsx": {
      "sha256": "f26e20f98d32158cee5fd633ec8bb13c156537a42954055c920bbc02ebe4720c",
      "graph_file_node": true
    },
    "frontend/src/components/auto-complete.tsx": {
      "sha256": "170988dc8a11147735ddd0f2b93a6adff771f95aa1cd6d4244269b235a8eea36",
      "graph_file_node": true
    },
    "frontend/src/components/coming-soon.tsx": {
      "sha256": "50d9ef37f17b22a93f584791bcc866df714362cf6d89f8f29a4765b724e40a75",
      "graph_file_node": true
    },
    "frontend/src/components/command-menu.tsx": {
      "sha256": "21c65ba4ddc02ae0006e0ff346eea90680a8d5f5555c3e3e4dfef9c438f26379",
      "graph_file_node": true
    },
    "frontend/src/components/confirm-dialog.tsx": {
      "sha256": "f517bc881c0003b64ee796ba604dae60378ddd8fad3a276a1de5a5626086ddaa",
      "graph_file_node": true
    },
    "frontend/src/components/data-table-column-header.tsx": {
      "sha256": "715e61878994e046550fbf4fde7a9d27a451c2e2113bbbf29f996cb0201f7f55",
      "graph_file_node": true
    },
    "frontend/src/components/data-table-faceted-filter.tsx": {
      "sha256": "f4012cc59912ee89b891777e84402059d64562ed63d37c00534197d764677bd4",
      "graph_file_node": true
    },
    "frontend/src/components/data-table-pagination.tsx": {
      "sha256": "33e22111032392ab56b8f7e971f94c90a278eedb791ab5a98def82c0c92a7b14",
      "graph_file_node": true
    },
    "frontend/src/components/date-range-picker/date-time-range-picker.tsx": {
      "sha256": "4f4a488d488d440db7ab59a5d027297571b9ea452926de7cedbac1eebf2edd68",
      "graph_file_node": true
    },
    "frontend/src/components/date-range-picker/day-picker-config.tsx": {
      "sha256": "ebb571315f7dc0bb45a5b814615315b5df2fcb6685501ed86d3b6d8202ccd7ec",
      "graph_file_node": true
    },
    "frontend/src/components/date-range-picker/index.tsx": {
      "sha256": "b2d850076c7e35844419f54234080752adbf9ae8e1f28c25944886cbc601ec08",
      "graph_file_node": true
    },
    "frontend/src/components/date-range-picker/time-dropdown.tsx": {
      "sha256": "d9f1ba1d48011057b317780e0d90419a7e759b200cd070fe757c6b3ee449e169",
      "graph_file_node": true
    },
    "frontend/src/components/date-range-picker/time-field.tsx": {
      "sha256": "26c66a360d0622596db0a3ae10a6d2a6b09ee793886c865bce3741f0e10bfa51",
      "graph_file_node": true
    },
    "frontend/src/components/date-range-picker/utils.ts": {
      "sha256": "97c4e0282f699ba269615bf6866ec7883c5233c8613273440d9a3867a86aa826",
      "graph_file_node": true
    },
    "frontend/src/components/filter-builder.tsx": {
      "sha256": "0bdc317f318a5caec79a00b2462fdeefa091b4d40d6c0f2c446315cab647598c",
      "graph_file_node": true
    },
    "frontend/src/components/i18n.ts": {
      "sha256": "be1519dcc65bff7b2077ab9c518f8505231b58787f83a939e13449d05631c7bd",
      "graph_file_node": true
    },
    "frontend/src/components/initialization-guard.tsx": {
      "sha256": "35cffa7763e762e57717794df8940cbea2d9b160a42456600e67f4509c26eeb7",
      "graph_file_node": true
    },
    "frontend/src/components/json-tree-view.tsx": {
      "sha256": "2ecb05dccd83936b8cb9b49937b8993d51fa0ac93a79d2f9ecbea6c2acfdf5ab",
      "graph_file_node": true
    },
    "frontend/src/components/language-provider.tsx": {
      "sha256": "8371533959f4459b9e638ffb364adb40b090e3bf9b469b00739c0bcece3930de",
      "graph_file_node": true
    },
    "frontend/src/components/language-switch.tsx": {
      "sha256": "c94596cd21cadcd7c00fde7f5191903dacb80d2c86a1848ce101794718896a32",
      "graph_file_node": true
    },
    "frontend/src/components/layout/app-header.tsx": {
      "sha256": "854ea7cc77b418378d679f90f2bf0263fcb816b8d79467e3258d80b9802e8461",
      "graph_file_node": true
    },
    "frontend/src/components/layout/app-sidebar.tsx": {
      "sha256": "420dd5c1deb9b0b34a491b7d06c04b91d2b3fd43e8fcaa3c9d64ba9cd998000d",
      "graph_file_node": true
    },
    "frontend/src/components/layout/header.tsx": {
      "sha256": "910033d308f74f09d08d8e3c5f361a6bdfa799cf5c2b18e7cbccec6e850c048c",
      "graph_file_node": true
    },
    "frontend/src/components/layout/main.tsx": {
      "sha256": "eae76611ae8193dd07c5d5b27aca6eeaf22ada62575029cbbca8e2296327705a",
      "graph_file_node": true
    },
    "frontend/src/components/layout/mobile-header-controls.tsx": {
      "sha256": "ac9f1aacf26fa472cb7c63d61f86ce5546510c09db4c7a11d8e4db4c1598c57b",
      "graph_file_node": true
    },
    "frontend/src/components/layout/nav-group.tsx": {
      "sha256": "65c42e0be93724542e466c592da6dd737b6f7ea026ee9f2391f936e7e816af74",
      "graph_file_node": true
    },
    "frontend/src/components/layout/nav-user.tsx": {
      "sha256": "fa67e332120103e60ab313b88d3eea324262ad6a568cf5f73393de4cb2639f26",
      "graph_file_node": true
    },
    "frontend/src/components/layout/project-switcher.tsx": {
      "sha256": "d4c5de42d3737832928e8bc395f007fbd86e40c465ed6076f0e55f4b4da7279e",
      "graph_file_node": true
    },
    "frontend/src/components/layout/team-switcher.tsx": {
      "sha256": "92695f4dd79ef29a22e0604c5c955eaac775ae07ce40e86b6c7645b2712a063d",
      "graph_file_node": true
    },
    "frontend/src/components/layout/top-nav.tsx": {
      "sha256": "20feef7924cca2c4037ce2031eb0c457ddb2519cf12b8d9b02635e353cdc949b",
      "graph_file_node": true
    },
    "frontend/src/components/layout/types.ts": {
      "sha256": "54eecfcd39c34deafe3f717bf652747d0b62bdd57c6845d8b33bb89469c4c656",
      "graph_file_node": true
    },
    "frontend/src/components/learn-more.tsx": {
      "sha256": "806ac70a8e7d33319495785ebbe0a95266f9fb7c813bdc85aeb7bff4f5c3cbc4",
      "graph_file_node": true
    },
    "frontend/src/components/long-text.tsx": {
      "sha256": "de6783963ef8af4fd9154ef1975e6f19db6cf09679838854f4a114f6897e9e8e",
      "graph_file_node": true
    },
    "frontend/src/components/model-price-editor.tsx": {
      "sha256": "f3a1e9a03998c939a0175e547b147f238bf9a516e54ee658cd27f6e26386b548",
      "graph_file_node": true
    },
    "frontend/src/components/navigation-progress.tsx": {
      "sha256": "3a531b898feaa4999818df1bd1eabbe945f677f40f828b737c18e42c1c1ae255",
      "graph_file_node": true
    },
    "frontend/src/components/password-input.tsx": {
      "sha256": "c736f5994e1e5a6487aac981ad8f4654995791b7070bc57da15dc372c2ec5156",
      "graph_file_node": true
    },
    "frontend/src/components/permission-guard.tsx": {
      "sha256": "614defaeeba2e6f6a84ed55aea9e1423d48585875888311f406689765df910e3",
      "graph_file_node": true
    },
    "frontend/src/components/pricing-display-toggle.tsx": {
      "sha256": "d54b5ee1265e6a4c50646b9a2614cb371f1e971c7f654cbe1b41a8ccd9bc9390",
      "graph_file_node": true
    },
    "frontend/src/components/project-guard.tsx": {
      "sha256": "52621e360a51bd582b2f3cbee82bbe91a21d4015c35053111feb8b83009aa827",
      "graph_file_node": true
    },
    "frontend/src/components/quota-badges.tsx": {
      "sha256": "42e95be2775e2d07b2de2a06e643628c3e84737f6b9983c2fa61ef8389f46d3b",
      "graph_file_node": true
    },
    "frontend/src/components/route-guard.tsx": {
      "sha256": "701a6ca5cbc3cf4f6002cd127a30df74c1b37e9cf65fc5155d398b47dd8f486e",
      "graph_file_node": true
    },
    "frontend/src/components/scopes-select.tsx": {
      "sha256": "e506308a5e6c54ee8954aa23993729eb517822cefb135170d249d1ee38a66654",
      "graph_file_node": true
    },
    "frontend/src/components/search.tsx": {
      "sha256": "b67bfdb7a66dc15b4078689ea3bdb30c4c137e1910ea6f080f79f4056ba4b733",
      "graph_file_node": true
    },
    "frontend/src/components/select-dropdown.tsx": {
      "sha256": "e39f52d63c070937a2176f8429e749e16e81d88fd8eaf4c11eaf81135ebd74bb",
      "graph_file_node": true
    },
    "frontend/src/components/server-side-pagination.tsx": {
      "sha256": "315c791c5fefa32c7327225345a471d57d2546c989194146a1fb317a8cee00ac",
      "graph_file_node": true
    },
    "frontend/src/components/skip-to-main.tsx": {
      "sha256": "649e4b8d141caab4c73ab2056fc04070b982d8a9ff7f26558401a9f4ca298e00",
      "graph_file_node": true
    },
    "frontend/src/components/spinner.tsx": {
      "sha256": "553f0a8817cdc40dba950b625b9b93a7f17138bb02ffdf8675a59508e02fdd9e",
      "graph_file_node": true
    },
    "frontend/src/components/theme-switch.tsx": {
      "sha256": "6d77451faebef2c2699539b9f7492a459a5e0333962b10cd2a5beb7c9a45cec5",
      "graph_file_node": true
    },
    "frontend/src/components/time-period-selector.tsx": {
      "sha256": "6013669e3ea25658c7d27bb6b098effc622f7e391ba8a06aad4614205b1cc92a",
      "graph_file_node": true
    },
    "frontend/src/components/truncated-text.tsx": {
      "sha256": "083e0e0c89cefdd5a1feb5b905cce7eb6f1368836b2fbb05f4129300da820e39",
      "graph_file_node": true
    },
    "frontend/src/components/ui/alert-dialog.tsx": {
      "sha256": "c9aa1723881710101baf90019858cf5e51724b5b678b048acf4ac4babae195e7",
      "graph_file_node": true
    },
    "frontend/src/components/ui/alert.tsx": {
      "sha256": "fe7b26e3b80cb3fbc98b0a5aa65ca753aacea834f3779d35bb93a66dca01dc8f",
      "graph_file_node": true
    },
    "frontend/src/components/ui/avatar.tsx": {
      "sha256": "a57421dec776e40f5f92fba2d08662e35ae83ae80e2d753ecd41e3f114e3e7e3",
      "graph_file_node": true
    },
    "frontend/src/components/ui/badge.tsx": {
      "sha256": "5d43f63b2409aa38d9aa0581a4185b534dbca98a24ccbf6957b3c5cdba673990",
      "graph_file_node": true
    },
    "frontend/src/components/ui/button-group.tsx": {
      "sha256": "666f2efc1b48427e0eaba49e2c319991d513d4599ecc2b2a43b4852f65e58f52",
      "graph_file_node": true
    },
    "frontend/src/components/ui/button.tsx": {
      "sha256": "c42546c8903369249349c54b4a39be3629e156edcd43efdf3d71a6557985e18f",
      "graph_file_node": true
    },
    "frontend/src/components/ui/calendar.tsx": {
      "sha256": "0cf73146aa85086f25eab3096fa185adababbd403c13e22995f0f4f3ffe97d5c",
      "graph_file_node": true
    },
    "frontend/src/components/ui/card.tsx": {
      "sha256": "d4a23c9936c7fec666680f2a0b544c9e0818de9ff5d94eb15792ab2ee91b301e",
      "graph_file_node": true
    },
    "frontend/src/components/ui/checkbox.tsx": {
      "sha256": "e94018e6b2db05af5c4b199b56e0653ffcef4d60fd6f93d529afce2ce8007c98",
      "graph_file_node": true
    },
    "frontend/src/components/ui/collapsible.tsx": {
      "sha256": "f4c051942a880861be46c8e6a0b02df914451f4e6d3ee324efc06a330e35e24b",
      "graph_file_node": true
    },
    "frontend/src/components/ui/command.tsx": {
      "sha256": "5685f99482cec9b36dca2df86bf17a15a01846cf5bf7e47c88483cf1a0c52dcf",
      "graph_file_node": true
    },
    "frontend/src/components/ui/copy-button.tsx": {
      "sha256": "de1cea3dbb0398897f6ca5822ee7b6e250f400ffd62d136f43fb2820922e371b",
      "graph_file_node": true
    },
    "frontend/src/components/ui/dialog.tsx": {
      "sha256": "a986c30ebc34fea5ddc7474743f57bc0c00c734412a215458fa13cd46c85456c",
      "graph_file_node": true
    },
    "frontend/src/components/ui/dropdown-menu.tsx": {
      "sha256": "ff924069c54e2b1883fbdfddb22bb6deb96ef534db2f54e7bb745303a9e17107",
      "graph_file_node": true
    },
    "frontend/src/components/ui/file-preview.tsx": {
      "sha256": "4c18c4dfb3233f3c553a7fbc4f62048774e1ff14498a7b85464b07f4a40f05f7",
      "graph_file_node": true
    },
    "frontend/src/components/ui/form.tsx": {
      "sha256": "4b64f4b04df43d00adccae1410beef4ffc5ae9d6b174814a629af0a468697812",
      "graph_file_node": true
    },
    "frontend/src/components/ui/hover-card.tsx": {
      "sha256": "cd6508476ce1ec04ce258cdc1679c310d64402ee7e7d55a325e6f7d81686cdc2",
      "graph_file_node": true
    },
    "frontend/src/components/ui/input-group.tsx": {
      "sha256": "3e14d2c7ec5c919244b9628bbde666b2889a04feb830cf77485ed0a6c47e9d4f",
      "graph_file_node": true
    },
    "frontend/src/components/ui/input.tsx": {
      "sha256": "7f5a4d5c94d9debfc32c7a28afa2a8ed51f6b6550b4027d52abff3d68c515c26",
      "graph_file_node": true
    },
    "frontend/src/components/ui/interrupt-prompt.tsx": {
      "sha256": "e640ec2a93a0d06000d4e5f09847292efcb08969780b4dd193bf57e0c50252cd",
      "graph_file_node": true
    },
    "frontend/src/components/ui/label.tsx": {
      "sha256": "7c2d2f86c83683fc580ae3a3c5d045e913b6fefad55288a11136c6ae193469cd",
      "graph_file_node": true
    },
    "frontend/src/components/ui/popover.tsx": {
      "sha256": "050c454d6b1d332cd8fc3518fa389571e5f73fc89eb79cf659f46dd7392843c0",
      "graph_file_node": true
    },
    "frontend/src/components/ui/progress.tsx": {
      "sha256": "998a5692a94aab45c54ff3563d7ab250e6c7664166065cd450199e6a1b7054d6",
      "graph_file_node": true
    },
    "frontend/src/components/ui/prompt-suggestions.tsx": {
      "sha256": "440c35c5b22c8d30573122acbf85a6e61215ff71f88a5aeb3f9b5264e136961d",
      "graph_file_node": true
    },
    "frontend/src/components/ui/radio-group.tsx": {
      "sha256": "21b74589a12a9666c153d46f3586d7ed184a3bdf2fd5fa38bbf7ab14c2507d36",
      "graph_file_node": true
    },
    "frontend/src/components/ui/scroll-area.tsx": {
      "sha256": "daecbb7567474731de5dd97750f922768ce09df216e01eab0f9f8b59a739b26a",
      "graph_file_node": true
    },
    "frontend/src/components/ui/select.tsx": {
      "sha256": "431c8fb257396753041ab6f8ebc52d21744339e6819f9acae123b6f65de4774f",
      "graph_file_node": true
    },
    "frontend/src/components/ui/separator.tsx": {
      "sha256": "b7c06b3775331229009bca756b3bc4691a319448c76f51f8eb675fdeed2e4628",
      "graph_file_node": true
    },
    "frontend/src/components/ui/sheet.tsx": {
      "sha256": "da52cec542ff788194b8918ba6af94de5da352ecf6dd6c2de09ae51925d808cc",
      "graph_file_node": true
    },
    "frontend/src/components/ui/sidebar.tsx": {
      "sha256": "39d8dca929b03300c01993d891ad44894a15b9fae939be69c954ce7acac678c4",
      "graph_file_node": true
    },
    "frontend/src/components/ui/skeleton.tsx": {
      "sha256": "aa757c77cae279a16d96f4496ebb9b50e75479231de02dd15f8b6b9ade8799c1",
      "graph_file_node": true
    },
    "frontend/src/components/ui/sonner.tsx": {
      "sha256": "83fe14d2e4432fdda04e8a4d1ea0cc8040b92d0eb03f06ccfe21b19d746d93f1",
      "graph_file_node": true
    },
    "frontend/src/components/ui/switch.tsx": {
      "sha256": "7c6e7be438a11bab94f597807c41e9bc08c3911b16f6a6e658b54b8a06fad0c4",
      "graph_file_node": true
    },
    "frontend/src/components/ui/table-skeleton.tsx": {
      "sha256": "3e6f27e5e742a3a01ff42acb7b8bd283be8bf67377ae3ec79a2f585421666e33",
      "graph_file_node": true
    },
    "frontend/src/components/ui/table.tsx": {
      "sha256": "024b64d7791d82ebc5d3db046ed589670ba5c2ad1a0ad635ab87bdb379fb1e08",
      "graph_file_node": true
    },
    "frontend/src/components/ui/tabs.tsx": {
      "sha256": "7b440b8c8eafc9b7a4c0ce1e4039828f2bd4305e559f7d410bc16944b2a3c034",
      "graph_file_node": true
    },
    "frontend/src/components/ui/tags-autocomplete-input.tsx": {
      "sha256": "f919dcb7d892debf84e232691d5056e9375e8e040c1e290ca9744670543b66b9",
      "graph_file_node": true
    },
    "frontend/src/components/ui/tags-input.tsx": {
      "sha256": "0ea2b9013fe5e61f6053da8d538e32316efad58e012cbc13386547d0d8bc765f",
      "graph_file_node": true
    },
    "frontend/src/components/ui/textarea.tsx": {
      "sha256": "1c7396b4a83f49217ca8fc55d8a7e9e82c934e0e05a35a951b422be030a8cb85",
      "graph_file_node": true
    },
    "frontend/src/components/ui/tooltip.tsx": {
      "sha256": "f8acad49085f757743c5fac7742e1c824684736caabcedf0e2195038389126ba",
      "graph_file_node": true
    },
    "frontend/src/components/ui/typing-indicator.tsx": {
      "sha256": "c86dcbf809b8c95989e4dfd0eb684d6c52d607250b9530ad35a2b93a8f4c0336",
      "graph_file_node": true
    },
    "frontend/src/config/fonts.ts": {
      "sha256": "7559f47ec079038e044f7d5e1d34d017029f573a8a4cf54cb64dc5a079d71ed5",
      "graph_file_node": true
    },
    "frontend/src/config/route-permission.ts": {
      "sha256": "78ad3f97771dbde4060e88b2e5381e9e4270221aed03f76b8c24908f46dd7760",
      "graph_file_node": true
    },
    "frontend/src/context/font-context.tsx": {
      "sha256": "f24a204cd9b0013416cb604dba854b5a4be3588edbbe9a9d4ab37274beaccbba",
      "graph_file_node": true
    },
    "frontend/src/context/search-context.tsx": {
      "sha256": "725e203b20ae60c222b1c11f68cb335f52ce039eea7596df3faebd8cfa04e07f",
      "graph_file_node": true
    },
    "frontend/src/context/theme-context.tsx": {
      "sha256": "ad321831379f7f442eaa98b7305a161980c140f2bacb0f0b840cb3beacbf2f9a",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/api-key-token-chart-dialog.tsx": {
      "sha256": "b8195014fb00c8e7690a3fb415d22296e54a585d3f96bf0a5d716cf9d464e21c",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-archive-dialog.tsx": {
      "sha256": "8474c08e7e390874ef2dcf9411482ea1e619cba3d5bb91da2ad363479241c85b",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-bulk-archive-dialog.tsx": {
      "sha256": "0252b0faaa4e314ac9957f2cefe1343bc33f9a6d3c2cb7746001626b91af0320",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-bulk-disable-dialog.tsx": {
      "sha256": "8c10c9d1db5e2720c5dc52606ff13368c0333ddca2deac142432e8336fd7c4a5",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-bulk-enable-dialog.tsx": {
      "sha256": "30046df4fc651fac4c96eb1f947ea9b79247ba0eb9fb1e144ae126d43b77e9dd",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-columns.tsx": {
      "sha256": "c76298096a92aae26b2ea9b4274a6e140a60651c8bb991eb1d888e1143798856",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-create-dialog.tsx": {
      "sha256": "7c794a80e6e8afcd19410b98d12a98ecb6689993a8b2da19604911a066bd027d",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-create-template-dialog.tsx": {
      "sha256": "38d5dfc70a765da3e62dcc822c5d6f44891f6e6013915d24cb7ff991fb857ea4",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-dialogs.tsx": {
      "sha256": "ea5d278cbda953669adf0d1db812b27fc8fcda4614968f506d1cc6ae1c06528e",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-edit-dialog.tsx": {
      "sha256": "e0c378f1e2eab2d9ac9ab2ba67ed4602992e63287a5d345e77ca6bca1184ba82",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-edit-template-dialog.tsx": {
      "sha256": "f9ecdfd6a725f3734f493fa3cc68918a7bc88d9ab6aff5304b123a4cbaea78b4",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-load-template-popover.tsx": {
      "sha256": "8f8d29df2352a5d5f38c0e6c078b3b3fd6f012581e659b6cae8b7bf21f9853dc",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-primary-buttons.tsx": {
      "sha256": "655ff179f7a078d84869859547434c7b34c76f57b0be861a763055e5746a2189",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-profile-templates-dialog.tsx": {
      "sha256": "350c9f75660ffefab1b0e7a1f9ec1de4f882d5073c238f6bf5a0688db2913fef",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-profiles-dialog.tsx": {
      "sha256": "09274a25a56b8bd19e7512198364afc87d8b94801262399688a996239f6d4a8c",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-rotate-dialog.tsx": {
      "sha256": "5c9228792590ef9dd146e24a844643a7f5425f8f92fa08e5ba6c6fc030c49917",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-save-template-dialog.tsx": {
      "sha256": "b0bd6a455126e7d89f2714991689ea1bc68ec1683543accace086393ea466bfb",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-status-dialog.tsx": {
      "sha256": "d64c1030f6a5030e7a67e8369fbb7f613b96e24a6e5dd57af5fae85557d577a0",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-table.tsx": {
      "sha256": "5f3c91a0037085f3a7b826f4c36e028e823f27acd64c63cf14155799f8de6fec",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/apikeys-view-dialog.tsx": {
      "sha256": "a3b38c565c4b578b2b602ef3018de8146c9130a9e64fca9a75fc0a1e4769c8a4",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/data-table-pagination.tsx": {
      "sha256": "bc5419e007949a6dc1b1633696eba1fafd69721abbf6af07d55d11e2a026e4ff",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/data-table-row-actions.tsx": {
      "sha256": "b20389152d6185ac2bc46861f34e6b926e4e3168426a4046cac498689a51645a",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/data-table-toolbar.tsx": {
      "sha256": "c80851a859ba13cc40ca2d3a1aa6a5baed62b9b61711c1e86b085cb6cd441a04",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/components/data-table-view-options.tsx": {
      "sha256": "d8d3a9c86dc840c46e7028b2ba9dc32d6150c38aa6043eb0f5d628767bcc091d",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/context/apikeys-context.tsx": {
      "sha256": "2dc0ea22a81773d4e92ccd6f2196481f9404b698ca77e5e0e8a848ec197c12dc",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/data/apikeys.ts": {
      "sha256": "6221b6ca05e0caa4ead5276ad1d25cd19731b57ce7b55c4ad02ac8b5f09db85e",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/data/index.ts": {
      "sha256": "ed9abcb9e72bbf0aced8d76b5c934cd1bdac352bd59a08d28adc50d79889838e",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/data/schema.ts": {
      "sha256": "ead82ed54d88b79c0bcc4b538bb93788a997eb1d748676ccd496ec483fb53680",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/data/template-form-schema.ts": {
      "sha256": "35d2d25a9823a6e38bd5d9addc7871b64389ff29441b9892c98ba554f5973844",
      "graph_file_node": true
    },
    "frontend/src/features/apikeys/index.tsx": {
      "sha256": "7d33698a8f6f69e2bf4b5dae846fdcfe0505fff79081027200a3a9cfe27209ee",
      "graph_file_node": true
    },
    "frontend/src/features/auth/auth-layout.tsx": {
      "sha256": "ed1d4a829617e4ae20ea617af9a30e4368ef40eeb5314e22bdc197b0872f5c88",
      "graph_file_node": true
    },
    "frontend/src/features/auth/components/two-column-auth.tsx": {
      "sha256": "3654a0ecff507fbb9be3550f8291933a25f71e1d2696f1edc629c838a00a996f",
      "graph_file_node": true
    },
    "frontend/src/features/auth/data/auth.ts": {
      "sha256": "1a202f5d38d802fce4ecdb0ba31f51d136a619de7f6c2e193df9b337b9c20e86",
      "graph_file_node": true
    },
    "frontend/src/features/auth/data/initialization.test.mjs": {
      "sha256": "94809cb433f2c89f97a6a9510f5956ee1a279ef93d47577fb6629a7e6de5412a",
      "graph_file_node": true
    },
    "frontend/src/features/auth/data/initialization.ts": {
      "sha256": "36bd9f706c8ab39b8a529be0fa88e6087f614caf7edaa75effb8a7bc0b0a2d73",
      "graph_file_node": true
    },
    "frontend/src/features/auth/forgot-password/index.tsx": {
      "sha256": "3fcfebe8de62d6aae2c2626d9d0cc6f3b40bdd7259ff37af2ab4bb2bd89dd268",
      "graph_file_node": true
    },
    "frontend/src/features/auth/initialization/components/initialization-form.tsx": {
      "sha256": "2efff8dab1340d7e354f6feea88d4373c3bcb6895f5e50212735b6fa4223262a",
      "graph_file_node": true
    },
    "frontend/src/features/auth/initialization/index.tsx": {
      "sha256": "0d7af4e9f08b955f87873976cf5907df0259e4ce48db47ee9e5d961100c3bfe4",
      "graph_file_node": true
    },
    "frontend/src/features/auth/initialization/initialization-validation.test.mjs": {
      "sha256": "5ea0574ad788bda7d38aa8bd2eb36f091d6c20b6434715baf1b48532c6c99e92",
      "graph_file_node": true
    },
    "frontend/src/features/auth/initialization/initialization-validation.ts": {
      "sha256": "42db357130d2852bcf447933088f01cbe315ec3af2edb335b26ee01a71cc232b",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/components/animated-line-background.engine.test.mjs": {
      "sha256": "fa76dadf2c06ba2b3a474998f035c3bf0e7e81b28917d57dfa944cbfa918760b",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/components/animated-line-background.engine.ts": {
      "sha256": "eba5cfe550de823ec4b7a8e1c6d787db975fe1f1c110f71912f66db02cd13d9e",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/components/animated-line-background.tsx": {
      "sha256": "6a1385264a68aea9be52c6747ef0c900c5465563defcfa9ad1e99efad4b6d924",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/components/auto-router-diagram.tsx": {
      "sha256": "03aefdf193994ea857c7d31e16d4a29c57ee13d7854b4b91ac2c4be24aeae791",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/components/geometric-background.tsx": {
      "sha256": "066cd835d96025b0135ac6ab902878695a1df779357602ea47400223cf6bfd19",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/components/user-auth-form.tsx": {
      "sha256": "620cd6e524ddad026daf3c29456ddb1f185e88449fde860e34263a6d699aa538",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/index.tsx": {
      "sha256": "bc7f7918930bcd734421c0049b8061f2d31f4cdb6b7fb53ea237be34aa79da29",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-in/login-styles.css": {
      "sha256": "3fd1c90dcffc577cc998519287a9d745a7ae27ab2c76985afee71fcf14f9425a",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-up/components/sign-up-form.tsx": {
      "sha256": "d6688232dfe45e4483085e8a9000c71c8cdb96906167799b301d27886761ded9",
      "graph_file_node": true
    },
    "frontend/src/features/auth/sign-up/index.tsx": {
      "sha256": "8c74aa5d25e1b6b91f45da9c48531ba4a4f95b51a1b66232ee8b06821f3938e3",
      "graph_file_node": true
    },
    "frontend/src/features/billing/access-plan-selection.test.mjs": {
      "sha256": "a94754b8267eebdb4274c23bd97ebcbbcc46a5e7fe026128410fdf42d8f12208",
      "graph_file_node": true
    },
    "frontend/src/features/billing/access-plan-selection.ts": {
      "sha256": "83b9442518f60300b261abdb2aa817d47007b92072f31177b1e33069531f646b",
      "graph_file_node": true
    },
    "frontend/src/features/billing/components/redemption-code-section.tsx": {
      "sha256": "1db9401c3ff11b7d1b7227c76223c8fead3022d24a8e4c86dd98c52449dddb45",
      "graph_file_node": true
    },
    "frontend/src/features/billing/data.ts": {
      "sha256": "1bfe82c65c384d4ff2cedb0e43d8b15f0765839f5b34024140bc8a1228fa39a6",
      "graph_file_node": true
    },
    "frontend/src/features/billing/funding-order-strip.tsx": {
      "sha256": "7fb74c3e7e038591e31b1418cc6f08b15bbeea759bcfce5d351e20bd029503d8",
      "graph_file_node": true
    },
    "frontend/src/features/billing/index.tsx": {
      "sha256": "ed159e169f4e6c2603bd4fb114cc4e1bc252b792c2d86ba6bdf62b344bfca1ee",
      "graph_file_node": true
    },
    "frontend/src/features/billing/quota-buckets.test.mjs": {
      "sha256": "c3b888badcc6a6868a0494ac6293d202cbe2d06ebae5522941ae691ceb145831",
      "graph_file_node": true
    },
    "frontend/src/features/billing/quota-buckets.ts": {
      "sha256": "a5c5ffab61c321f9929e554499c610c439ca7832193c324ffc49ce8bbf7433d8",
      "graph_file_node": true
    },
    "frontend/src/features/billing/quota-ui-contract.test.mjs": {
      "sha256": "a070979bc8305128b80f7bd8fed4609697b21d3b36fbb10e047e3787a1a46d8b",
      "graph_file_node": true
    },
    "frontend/src/features/billing/redemption-code.test.mjs": {
      "sha256": "c8cd819b6e9e7316e51bd5d53380249f395774e3f398c96b113b61e219073a0d",
      "graph_file_node": true
    },
    "frontend/src/features/billing/redemption-code.ts": {
      "sha256": "548abce2b42c1db0cc1f80834204eda3d470c6bd4b65836f5077af34f7363723",
      "graph_file_node": true
    },
    "frontend/src/features/billing/redemption-data.ts": {
      "sha256": "3440da65575523dec241aa7bcbfff3348f8fbcd5e8ac1ab1ed92ec866d033c53",
      "graph_file_node": true
    },
    "frontend/src/features/billing/redemption-ui-contract.test.mjs": {
      "sha256": "bc3841521969872abcf01e35ab67dfeaf7d9f51383aec4a08933568fceef76e8",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/change-set-display.ts": {
      "sha256": "88ec9a93bbd4cd294ccfebe6d3700bd669e8ded875b36c0c31902a952703cb36",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/change-set-page.tsx": {
      "sha256": "48e6262a17f7d3e623b2264d24f1577ff1773e725fb698215d2a333dd53e107a",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/change-set-search.ts": {
      "sha256": "4982374bb2bc16787da23354ae55a78a5eaf3cf578703c82388628649db53a04",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/changelog-page.tsx": {
      "sha256": "f3b611c13e9febe81ae16062ca285ce42a8d9981f7d644def7329eafde6cdcb1",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/components/change-set-actions.tsx": {
      "sha256": "72ee2213c912ae770d324ad874d4198ededa390ca8e984b78653abe86192c57b",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/components/change-set-badges.tsx": {
      "sha256": "c01a202a91b09335cfca6d928b55ffe38341b721e7a307b9d1eae7946e272986",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/components/change-set-detail-dialog.tsx": {
      "sha256": "0ebe3f62c1af1eabf1bc91e52be7ada7ab2be8438029b9b72c2435912c03c6b0",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/components/change-set-review-dialog.tsx": {
      "sha256": "5b772196e98d168bbf834e4dc24ff2cc5c9fdda0655b921455ab257c3e0e8a92",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/components/change-set-table.tsx": {
      "sha256": "a0223180182e8d07ec94269645ad42d8b35afdb82f0c76deb45c9f0f08e3663f",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/components/change-set-toolbar.tsx": {
      "sha256": "a7ca49c25360ac0900b2cc9d80e8b9c7e976d7a254568c9c01e462fc0d3b5440",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/data/change-sets.ts": {
      "sha256": "2b02b73e7a223244d719b55e50557e476954ae2e91b6cd893dd1a97ff91903f8",
      "graph_file_node": true
    },
    "frontend/src/features/change-sets/workbench-page.tsx": {
      "sha256": "b131c451ae8c23a5d084d23c523e12c87d4b9af106eb3595598d23cb13ed5f4e",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/atlas-cloud-icon.tsx": {
      "sha256": "1c2871c6fac888d325d7a20f8282374e10ff4925e6c94d81f3ef43a545f9f771",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channel-expanded-row.tsx": {
      "sha256": "12929a43058c2a75567a4097b2ce8b172e8ce475268c6e8aa424c69c88b35cd1",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channel-health-cell.tsx": {
      "sha256": "1d149eb0a46db13a59c58ab6935896716ade394c38d1f1630d4cff31083753c5",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channel-limiter-cell.tsx": {
      "sha256": "27857f108980a488d900fd2a19f2d4132bd59a915225088dc41a1bd317aaf0e2",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channel-management-adapter-badge.tsx": {
      "sha256": "21882718c52274866769739119dcf925f61940c64cccb03cfbb34baba20ddb08",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channel-operations-workspace.tsx": {
      "sha256": "dbdf825c0b8a8fc6b8ca3014a51ec233b7295ea44b4bfbacbe91af15d902efb8",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channel-overflow-menu.tsx": {
      "sha256": "f4db2a0183fad86dab6f2a3049a6dee3c29a679f8daa6f0670a480e4e4038c55",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-action-dialog.tsx": {
      "sha256": "bc48c8822f200795cc40958aa68572febfec3c8a7d855400a436ad18c87a2781",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-archive-dialog.tsx": {
      "sha256": "dcf372363e52fee2fa299e842edc61d38ec226059222f112b4085fedc3fca942",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-automation-dialog.tsx": {
      "sha256": "dcf1c27d8fcf69c2874b8072d58145dd9636c5739fc66278ed28ccbd39fd6cbf",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-apply-template-dialog.tsx": {
      "sha256": "08981ba1c121f7b3c3bea17abd0264bac96dae1734eaca0ce403250434e8a045",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-archive-dialog.tsx": {
      "sha256": "780bc8b52eaa11deb63262db27d0288cd43f201522de720dcfcf9947497245d8",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-clear-template-dialog.tsx": {
      "sha256": "e04245b801ac13e497192badf022abf5579b3dfced18e20bc0abd26fbac63588",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-delete-dialog.tsx": {
      "sha256": "6ebbc35bb0fa6f62683e4e0192834d6d7a6a1c6191e58c934ec4920dce1c3919",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-disable-dialog.tsx": {
      "sha256": "a28218c769a8d464d12c8ce00b9d9b73d3afdc1a8b52a82b5837b3c158d1c1d8",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-enable-dialog.tsx": {
      "sha256": "0e4333bbd4ed38c9c7b1b3ca63c2c28945cff9d9bd8878686c1f50e5834356a5",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-import-dialog.tsx": {
      "sha256": "a657c145318689998c3ff29b8fd7eab6e3b431a9aa8bea9d07aab0dcf63e726b",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-ordering-dialog.tsx": {
      "sha256": "8643ab28334eca733e97d91e373cfade2aa05eec998c245633e6540ed1371b89",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-bulk-test-dialog.tsx": {
      "sha256": "5dc786934df5b206f6432be1948cc53bf2ac7288353abf8091b0504a6a408d1c",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-columns.tsx": {
      "sha256": "4cfdd1a2b578427997a567739a6c97420177cfe7a60bfe6cbbf0f0deebf05884",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-delete-dialog.tsx": {
      "sha256": "236a18958ef35b6ced21f072cc146409fa347c5e9d970f03f3ca992d01d19aa6",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-dialogs.tsx": {
      "sha256": "88e9caa165243c6deadb3290a9eab90fd33cf966d96451fad736b6c8f0878f54",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-disabled-api-keys-dialog.tsx": {
      "sha256": "28e66520aa9a8398a2592f4dbb5d648c4aa2f8bef2b299fb8a618c019b6950fb",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-endpoints-dialog.tsx": {
      "sha256": "3a6a6823228e2ef2ae05d31a67c5d267aceaf8adad3845f5ab067801361bd6e7",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-error-banner.tsx": {
      "sha256": "1ae3516314a16f9c4f1ac7e463de98f2f976248ffeba321ff458095e929743b6",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-error-resolved-dialog.tsx": {
      "sha256": "b908be969608205ae5c1884de0223bf4c97a6cc0d4adf7c68aa47e55c166e17f",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-model-mapping-dialog.tsx": {
      "sha256": "4bc616293116758c8397f6d1875e3bc7342cfd7827c752e7af3d95e564ab9137",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-model-price-dialog.tsx": {
      "sha256": "06718d168760ec684a537de8b5177b0e4c9be9cea7134d07da8036c5f9368ab7",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-override-dialog.tsx": {
      "sha256": "f69719080c6e01148b111aef218afe68f288b9d3a46b1d2bb80095b396c9c9ff",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-primary-buttons.tsx": {
      "sha256": "820b491d0c6b3b7c36a505d942d03b6f3e66d4148077a5b3a10bf4d7b2aa77cc",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-proxy-dialog.tsx": {
      "sha256": "01964069cbf9ccc93b3d7566f5135729eee1fac7450e924737a7f634421626bf",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-quota-probe-dialog.tsx": {
      "sha256": "c7905c7e1fdd9043d839d03e4953e8a62ae3e465d0e2648857c23b4f0a21edb7",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-rate-limit-dialog.tsx": {
      "sha256": "193e3cef98f95f0fd753677d2ecc52e5832972d71e7e52c3932978376c7eb552",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-status-dialog.tsx": {
      "sha256": "42486668a6dc7be4d12c2699a82ac96fbd0f578bfceefd79a66c557e7b5258d6",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-system-settings-dialog.tsx": {
      "sha256": "c30610ce86788da71cb2f9418b547886ca535985b467e9ad9068b07394862cc0",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-table.tsx": {
      "sha256": "99d0513452a35993b5b636c07257108c79bcce60ae00ece257671096d1ba25ab",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-test-api-keys-dialog.tsx": {
      "sha256": "850943c8aee1c3e70259b6406a6fd19f9f1fa8c1ebadee8c5579163c64a1e32e",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-test-dialog.tsx": {
      "sha256": "2024e30f880b468f18e735d766d5040c191325e45145e422c8fea10ac8ddb75a",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-test-history-drawer.tsx": {
      "sha256": "f87fc6d8de47c222bac0c8e313484920fd177daeb6ab224a4064440e7113e4c5",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-transform-options-dialog.tsx": {
      "sha256": "b003936b255a5410887bf35a3c33298e7663f318da0619dbcc38449b9c5f3a9b",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/channels-type-tabs.tsx": {
      "sha256": "57ebe166286c972828faaf213fe888bcab1f6e7f4506d6b0c0bf8d0165ffd60d",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/copilot-device-flow.tsx": {
      "sha256": "7a4552d4e501fc4dc46a70d2f3278791efca6b8321aa1140e0bcf150561ac35b",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/data-table-pagination.tsx": {
      "sha256": "bc5419e007949a6dc1b1633696eba1fafd69721abbf6af07d55d11e2a026e4ff",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/data-table-toolbar.tsx": {
      "sha256": "55fc492897874cca6b7848384233912ddcd780735812769365ef967e296d4a9d",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/data-table-view-options.tsx": {
      "sha256": "400d926de2506318189f27a1313b43a18dfc29fb911790635a04a06f1daaa3bf",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/evolink-icon.tsx": {
      "sha256": "d268f170802e5b76d4f72569d9a08170a4bf2034e7d397398a33f909afbc6f15",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/manual-model-badge.tsx": {
      "sha256": "454afa4b5b602a305ad36644bf3a89ae320982ac8d3f8bffba0c04180d3fd8ca",
      "graph_file_node": true
    },
    "frontend/src/features/channels/components/nanogpt-icon.tsx": {
      "sha256": "72628c8e2a40595910d75338211badec5fc955069d1cf2483774b2167aca4821",
      "graph_file_node": true
    },
    "frontend/src/features/channels/context/channels-context.tsx": {
      "sha256": "47d894cd1e220786edbffec14c41c53db4f220d6f72f475539cc5f9a4e93dbce",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/antigravity.ts": {
      "sha256": "38b559d40399889113da9c239d7412e3ed4d62523ae18e5864e5de59400503b5",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/burncloud-models.ts": {
      "sha256": "9392bd1f86a95445b16b84bea4f61203e86ea241fa9f1bcf3b6679346b368d35",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/channels.ts": {
      "sha256": "b0328e73672bf19d3884225424a137a2837e1d365d487b409c9b2c3ec301c08b",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/claudecode.ts": {
      "sha256": "d6ec28a9b1a8e00ba7b5b25fc209af8ae478b07c8c6360e617d052e104bd8305",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/codex.ts": {
      "sha256": "a2685189116fa6acb49c79fa0202d0c1a5143a2b730ab0711e5b1acc4b3a8afc",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/config_channels.ts": {
      "sha256": "8df3c6fc194cbd8f0a4e3378fa76536baef80247b1671f545048a29c8227a67d",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/config_providers.ts": {
      "sha256": "c29dad02d9fc4d6bf19f726fad17054f43222a7de88fd317a1cfb031a77b7e45",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/copilot.ts": {
      "sha256": "065c720a52c199087f968bdab826a5687f6c8d1e17e6ba187adb7186833431b8",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/index.ts": {
      "sha256": "bb2a16bfc29019e57eac1ae2e0033328a79f7dce504f4e7c5d02e165c3bb1303",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/schema.test.mjs": {
      "sha256": "1ccec8acf54dad807c5ec378469163b902ef658de0743569973b8746a03901b6",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/schema.ts": {
      "sha256": "0c574c342d634b169fd16134d0d74170844f49130db4f28b0e249a6bed48606b",
      "graph_file_node": true
    },
    "frontend/src/features/channels/data/templates.ts": {
      "sha256": "50f4b360f6216307ad3a41bcd467e73702c1a068c6ad7bfa3f0099003a103b18",
      "graph_file_node": true
    },
    "frontend/src/features/channels/hooks/use-device-flow.ts": {
      "sha256": "44a201564d8cbaa821fe43f0641c64a09c9d835db131e14780cb012abd165b52",
      "graph_file_node": true
    },
    "frontend/src/features/channels/hooks/use-oauth-flow.ts": {
      "sha256": "ddb1065e08cac101cb570abb4b5eca87d06fcf4077fb0d72658232182b54fce2",
      "graph_file_node": true
    },
    "frontend/src/features/channels/index.tsx": {
      "sha256": "0441e6a188ba980cd36f8e2e439d7af8bb9bc7348e88e675b5f8d36710c8a2c6",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/channel-filters.test.mjs": {
      "sha256": "92f267bddf86d2c2be4931f8b2619f6ace5c65651e5a237ac39db37a2c6c5438",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/channel-filters.ts": {
      "sha256": "e12a7e0b355e571c0ee2ac98941476810e796effd85ddf5d2ad25ab477c829e9",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/channel-management-adapter.test.mjs": {
      "sha256": "05bf4579a697e801b3933dded28c2434746d7c00cc4da742f8ce9950608730d1",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/channel-management-adapter.ts": {
      "sha256": "c6ffdd2112e3fe774f206a3aced180d9d1f28793e6cc294ad8fecf3633998522",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/error-formatter.tsx": {
      "sha256": "eb567688fd4e1ef5d6d0805f3342c7c0045c6fb399d8e63311e3a052a78883d3",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/failure-classifier.test.mjs": {
      "sha256": "92d0eceae54c10e41b199a29dbbf070d2d97fb4228fa041776b3cb18d8b7bdc9",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/failure-classifier.ts": {
      "sha256": "91d12d6e017acda6b8f53d2e0b40e60e2c2430af8c9c037857018e13522b969e",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/merge.ts": {
      "sha256": "c2ae8fe8c2f36b285686971b376ee709785ce5c0cfc1dd17eadb44d478a106df",
      "graph_file_node": true
    },
    "frontend/src/features/channels/utils/pattern.ts": {
      "sha256": "9aa01bb78644733ad51710e7e2e2ed055ce90ef333f057fcb7f35b24c28b569e",
      "graph_file_node": true
    },
    "frontend/src/features/chats/components/new-chat.tsx": {
      "sha256": "2c96e30eeac59c052c3b67ecdbb5542efa0866af7b013ad4ed95efaca7799e1f",
      "graph_file_node": true
    },
    "frontend/src/features/chats/data/chat-types.ts": {
      "sha256": "fe742b42f87794a3c55ed25d7a920ae17be7b23c4bb0dbe6ed067ed63e486f0b",
      "graph_file_node": true
    },
    "frontend/src/features/chats/data/convo.json": {
      "sha256": "da977874ccd9d4fda2c45de5484674387b49e196b1e622058e51f93d45f7b740",
      "graph_file_node": true
    },
    "frontend/src/features/chats/index.tsx": {
      "sha256": "66ab8ba5fb3a29f63ebdce7ceeb75062c89b076a063b210cc053647e72c066e9",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/channel-success-rates/index.tsx": {
      "sha256": "cedd444ee6e529d0b377738f01f88b8c2767a1d89a4fea9de7b2c2ea0ff2fc9c",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/channel-performance-stats.tsx": {
      "sha256": "67c83de95fb863ab5c3af979901a6777fc1affe45de70c7cf60bc0282f33686d",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/channel-success-rate.tsx": {
      "sha256": "7b79f5e57e81478696265b03e066d9aabb1bd52faf2120ba6315cafcd0122d67",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/chart-legend.tsx": {
      "sha256": "6a594d4bccb488e51d7e1ba5e486e9ba78a1aa175a8868a9a3c1a5a461bbf57a",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/daily-requests-stats.tsx": {
      "sha256": "c3807e1cc59249795619f2048cfcdec7083a513950de938df95437bc2416720c",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/fastest-channels-card.tsx": {
      "sha256": "91b9dc7fe831f71bb9b86a56584b1e0964ff6786b7a56b10efedcdde854d8321",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/fastest-models-card.tsx": {
      "sha256": "d1878274e646ccc9e5922f8af01885bc8cf446c9a9cd990b82a232a73b0f1f90",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/fastest-performers-card.tsx": {
      "sha256": "152e4561979ad9263a72fb63b6092c5653ab4e671bf48f000f7bbcb01401f487",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/model-performance-stats.tsx": {
      "sha256": "15a2f295022e0cf64f8287e1ff79b142f00ed0ac8181c5a6b5757ef56d22fb71",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/performance-chart.tsx": {
      "sha256": "297971a2fb01db1ffa63dfdf61c84e738519d3768cfa1a3bff30ec767711f03a",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/requests-by-api-key-chart.tsx": {
      "sha256": "87f68924d0a82e0540cce9870c7e515897d413389b693e3d565497dc3cc6e325",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/requests-by-channel-chart.tsx": {
      "sha256": "65df7ba62b013d514bfb02cb8292254dd09182a8ccd57e98efcf93975afa37a6",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/requests-by-model-chart.tsx": {
      "sha256": "b1e6da109ea0b297644f54db81e71a625715ec731a096d45f3fbca6eab6eb5a8",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/requests-by-time-card.tsx": {
      "sha256": "c61c2ab608ba1f757a0ecfbc38e5abd2a8f9624da825a2fbf60234071fc6ab67",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/success-rate-card.tsx": {
      "sha256": "d0b7e2a573911df4148e19bcce0c7dd77f841a40a99c54d6adf4b91749364de7",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/today-requests-card.tsx": {
      "sha256": "2be05c45af85c27ef3dc37e3b3553c7505247837355ca49b06c12e3b6b95c09a",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/token-stats-card.tsx": {
      "sha256": "ac7efa83a20b87162e23a104c36fb99e1ece6f62c3707b3ba6b4f1c16971c8aa",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/tokens-by-api-key-chart.tsx": {
      "sha256": "62526ee6f9a358a3c8d8210eb30035976d8f9923a5579126465f25c42e106fdf",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/tokens-by-channel-chart.tsx": {
      "sha256": "10818a7b02d8b68fdd3086d1b869b7b2da4ea0a1ca88141e277f0de8493c2c01",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/tokens-by-model-chart.tsx": {
      "sha256": "447c7fb8273d8cd0bd164488104c602d8f7b247c8fadd6dceb2c96f2cce816c6",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/top-projects.tsx": {
      "sha256": "b58076a847aaf07e2f85f90e95119ed81d58e686db349cf518b9d15a1657a1e7",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/components/total-requests-card.tsx": {
      "sha256": "5592a1b2de8ab21a85ba14444bdeae10cafed7a71b15601bea474e0da84f5aec",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/dashboard-animation-contract.test.mjs": {
      "sha256": "f88b43a3b2006bf1e6bd77aa81eb737eeacf51394c03c07bd7382c3fedc08f64",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/data/dashboard.ts": {
      "sha256": "a2f79b34424bee3d18c182fa686139b3a2794e9cc98d92e3b8f9f6549f142b3a",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/data/fastest-performers.ts": {
      "sha256": "10e8e2432c325ef1612f29d1776d1badba83d098e34026daaa43c0b37fb92a53",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/index.tsx": {
      "sha256": "d554c554cb7f33a62ad853060ca027458bc2ffade14048c55fe35395449f9ac7",
      "graph_file_node": true
    },
    "frontend/src/features/dashboard/utils/chart-helpers.ts": {
      "sha256": "42b978b33417e33c1979eac467ef0bbbd9481d3c7fd5db4452ad3dcb1257603a",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/archive-data-storage-dialog.tsx": {
      "sha256": "dd26b5c8266726f9d896fa4cbdc66d837a627f0c4c0467b4d23a4ae21081d3f4",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/create-data-storage-dialog.tsx": {
      "sha256": "5f2b18f4c537698bd861b627d4dff345183b65d7506f291233b5faea9b1448c2",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/data-storage-actions.tsx": {
      "sha256": "8a2770029aa601fea420a1869347f1794ae2772541173ea62fcad30525cbbb86",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/data-storage-dialogs.tsx": {
      "sha256": "04b263becdffce2ed7b77c6abfb95508b6d669e7b6d259fa26406bb7c8f88eb4",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/data-storages-columns.tsx": {
      "sha256": "d9d473b711d48133aca4e215ee1155159e992971094fa744f73781f744014cf4",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/data-storages-primary-buttons.tsx": {
      "sha256": "2cc19f139ee5f1b5361a2b4d4b531e62ec22f1e9a51d7cf450d3d4634016a2a6",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/data-storages-table.tsx": {
      "sha256": "52547232a53b85b6349dc69d81e20eb2f1bcce3279bd87c63640066dae5798c2",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/edit-data-storage-dialog.tsx": {
      "sha256": "584bde01400caec2ebce1881c2b5096213abdac449ad15bc4d8f17a04bfe1252",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/components/types.ts": {
      "sha256": "7cc03e7daa2f25720ae4cd6de7082cbd969afc5bf882c57cd37f091c0349e4e4",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/context/data-storages-context.tsx": {
      "sha256": "4884b6db7ee547ea3f77cc6146806169094e62884075891ec999a3e4f4a3fa6c",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/data/data-storages.ts": {
      "sha256": "1ec49953f5da3b2ad1c973aee84eb90dbcba5137eefa91ffeab7c3b4838d3d2c",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/data/schema.ts": {
      "sha256": "aaa8da63b57ad2d0531fb983bd9b20c1bfb532334931bd77282e682250a5e4bf",
      "graph_file_node": true
    },
    "frontend/src/features/data-storages/index.tsx": {
      "sha256": "16fa3b859e2af3f93a738879833a160728f79e296ea8b9c4b0209941fa192dbe",
      "graph_file_node": true
    },
    "frontend/src/features/errors/forbidden.tsx": {
      "sha256": "b6c622333f14e63ba4edaa3ed38803ece5c56d68a2e78c6018a288669319a40b",
      "graph_file_node": true
    },
    "frontend/src/features/errors/general-error.tsx": {
      "sha256": "260500838c889162498f2e3ec0bafe63b255e4f8693a9cfb911fbb3ddd6e418a",
      "graph_file_node": true
    },
    "frontend/src/features/errors/maintenance-error.tsx": {
      "sha256": "184b629eb223048735fa2ad886c474c6876464e994925e857be345c04f857cb9",
      "graph_file_node": true
    },
    "frontend/src/features/errors/not-found-error.tsx": {
      "sha256": "ab185d49d2e3770a4bfac4fac288083f456da8a589a3f0e70060b39b035312b6",
      "graph_file_node": true
    },
    "frontend/src/features/errors/unauthorized-error.tsx": {
      "sha256": "4ca8b8e07548a2faaa4e0a4f9f55d43029097a56193db20513299356018cd446",
      "graph_file_node": true
    },
    "frontend/src/features/model-market/data.ts": {
      "sha256": "8e1285bc7a9d1de3f2f89eb8d905a9b3cce6debcfbe417a8fa1767c2f5ef984c",
      "graph_file_node": true
    },
    "frontend/src/features/model-market/index.tsx": {
      "sha256": "baccd2649b3de090889d559ef6a51c55beab2b1a3c3a42071f8512ce43c10ac7",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/channel-models-list.tsx": {
      "sha256": "ad0095c6c549b1f00f2330e3f256cc859d1540e48cbc938077d68d8e55ea0723",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/commercialization-panel.tsx": {
      "sha256": "6e17117ac6107ae73caf6ef63018fd72da89ed63082123d2adc90dda58997fee",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/data-table-row-actions.tsx": {
      "sha256": "87119e6a06efb29ac02878ae7bb447370857ce2e857d263001587b39ea4c894f",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/model-catalog-health.test.mjs": {
      "sha256": "0b6997c189143d0469d2acc809d0e04a27ef3306afc6b6b717b15646278979a0",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/model-catalog-health.ts": {
      "sha256": "ee5d676ba4aea5f86d61ce228bdbae732e16653db6ec3f1892da94c18e564fa5",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/model-catalog-pricing.test.mjs": {
      "sha256": "cac83b3f3ba9faaff5a736e314d8f7d623bd2ec20fa66c5fd6eea7715647641f",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/model-catalog-pricing.ts": {
      "sha256": "68fba5f675c739d46c3d881f5adc39bfa610b2d25215d630dfc0847f175e954c",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/model-catalog.tsx": {
      "sha256": "41d2e9c6c03081cad6734c9a0609f3e0b807c2ccab1c13e5ad465a63b510994a",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/model-icon.tsx": {
      "sha256": "ee381ad6d48f0d2059d069b3d64a76f9194c381aa8c3a89b673e3a1d99cbfbdd",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-action-dialog.tsx": {
      "sha256": "b5e587d56f361684ed9f1718a2163f929f1f90c0d2507774b103cfbed5aaa985",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-archive-dialog.tsx": {
      "sha256": "7c4853a08bce7cb8b2631b5b6da026cec54a9265a3aa9762700df4f71b2b38dd",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-association-dialog.tsx": {
      "sha256": "ee0b0518157ccb7b8f750a69549f984e8c5ed4347619af7a1c75a16e260265b1",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-batch-create-dialog.tsx": {
      "sha256": "c2fa4ad03b5c399031ed8825d3360354634341ae5537b171a670b3331bdea7aa",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-bulk-disable-dialog.tsx": {
      "sha256": "5f397feac929b6baa4ab964640f05d9b282e0acea308492c7fdb31860932b2f5",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-bulk-enable-dialog.tsx": {
      "sha256": "f6151d48e65d332b26b10fecca46c9dc241a6ea9024e27426e0adcd36557c739",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-columns.tsx": {
      "sha256": "11136884c77f4cfe808474b280afa318915a5d2dc1138f471ba3357d22013e6f",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-create-dialog.tsx": {
      "sha256": "267edec61944184cc3cc46a957e789abe9c3531d4cf2ea68c698049ea7178385",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-delete-dialog.tsx": {
      "sha256": "10ec943002b27a8e0fde988a3689b1f42e8acb0ae3bd2acb8347cb6354838c81",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-dialogs.tsx": {
      "sha256": "3a0f03df94bacb5f6a7935547456731a4ee2d73a816aa76293bb5e7e5f053ccc",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-onboarding-flow.tsx": {
      "sha256": "da3ef6a9079fdb851cbf0adce11274b87dfba80974d19bda60d4a70cc1522502",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-settings-dialog.tsx": {
      "sha256": "a7008477e00430e886ee182a21cba92d816b37fb9a3d59b1ff0825e74ab306bf",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-status-dialog.tsx": {
      "sha256": "316bef8915baa89530abd3e465c6248e39ac0be6dcd8b38966d99709f972157f",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-table.tsx": {
      "sha256": "d11fd803630c8a1d3cc3d13287efba8423f2680c3af4ab658845533dab9da2c8",
      "graph_file_node": true
    },
    "frontend/src/features/models/components/models-unassociated-dialog.tsx": {
      "sha256": "3e764944662dc0ad3efb2be614487e7a754aa8d600c7acbfe1fb8f527e80239f",
      "graph_file_node": true
    },
    "frontend/src/features/models/context/models-context.tsx": {
      "sha256": "9b91554d86957aa80a5fb19b04d8b4e51a7fd82196df588c327d522adc02e2ae",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/catalog.ts": {
      "sha256": "3e2139bf3df389a6a1e63054fc8d34f8f2302afb75ea42890a9b02aeeb5a9b56",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/commercialization.ts": {
      "sha256": "561fe0c6b6bae10227199e11826e12445f44afe3bb47d504d3fd4aedfefb672e",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/constants.ts": {
      "sha256": "ef2d76c4e97d8ee76d47386fd1c997692de90add1cf0c66b9a1a4afebdcd5aea",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/models.ts": {
      "sha256": "0dbf743f2c49d3459e074916d26bbe5f62282f7bd1a6e082d9013edf8eee21d0",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/providers.json": {
      "sha256": "535438da08b8832ef24a3639e180981ed6ec576d21f28442e837bfc5ae266d47",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/providers.schema.ts": {
      "sha256": "ed39fae7e69c6c2faf00b5d5182458b497599466f83aa721f925c95ba4511b8a",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/providers.ts": {
      "sha256": "cc969697f9b89ba75c3263f512369545b7c20723759e7426fcc488e6cb798f61",
      "graph_file_node": true
    },
    "frontend/src/features/models/data/schema.ts": {
      "sha256": "0ebea528b3363db1dc784b08488f5cabe26e0bc0ad7ca3bf7504b26e7e434fe4",
      "graph_file_node": true
    },
    "frontend/src/features/models/index.tsx": {
      "sha256": "fb24d5fa479ec476eaf57568b189847076c542db5a17d99ecf40885c669aeff4",
      "graph_file_node": true
    },
    "frontend/src/features/models/model-search.test.mjs": {
      "sha256": "3ce7f463303b21ab5f63cc1be851732944a2c534b95e662ab59e0e50cae66169",
      "graph_file_node": true
    },
    "frontend/src/features/models/model-search.ts": {
      "sha256": "d785e59eaa3f24e3f3e381ba7ebd953a30390a4eaeea70eb745c394bdfabc9a2",
      "graph_file_node": true
    },
    "frontend/src/features/onboarding/auto-disable-channel-onboarding-flow.tsx": {
      "sha256": "295cd7fce6201b949df26c2dbe19f230f5d0a996fecf59e5d5dbc0b40997a8bc",
      "graph_file_node": true
    },
    "frontend/src/features/onboarding/financial-setup-onboarding.test.mjs": {
      "sha256": "33c7a5d2e706a905d42851bce9180d79169bf3f727f7ea4ae908727bca4eb675",
      "graph_file_node": true
    },
    "frontend/src/features/onboarding/financial-setup-onboarding.tsx": {
      "sha256": "2c2474798baf5eb2c0db07a063dbb74fdcc0d1c09937777afb57b78d30cec021",
      "graph_file_node": true
    },
    "frontend/src/features/onboarding/index.ts": {
      "sha256": "8ef8d07c605740eb101c2a83b8cfb2b1d316bb0c183f32fb913b1f83ce2ebb81",
      "graph_file_node": true
    },
    "frontend/src/features/onboarding/onboarding-flow.tsx": {
      "sha256": "f77c25a5e33319cdcd2d066c81db33a25dbfacf12dc3717eecb823acb6419df9",
      "graph_file_node": true
    },
    "frontend/src/features/onboarding/onboarding-provider.tsx": {
      "sha256": "dd0b5f13aefd684bbc02e34a973e9c343bf8335b2117dd29911d712fabca1112",
      "graph_file_node": true
    },
    "frontend/src/features/operations/analytics-views.tsx": {
      "sha256": "b9096185635f007d4d6964172692d5e7774875477364792547777a6dc6188d30",
      "graph_file_node": true
    },
    "frontend/src/features/operations/analytics.test.mjs": {
      "sha256": "c9d1450072647ae5b0f722a6b8d891195c64d3ee3c2482135d708d38492774b4",
      "graph_file_node": true
    },
    "frontend/src/features/operations/analytics.ts": {
      "sha256": "13debdb601164330a9ff8c6d8234730ca46f31ba02070d8c775b516cd3f85513",
      "graph_file_node": true
    },
    "frontend/src/features/operations/chart-settings.test.mjs": {
      "sha256": "be6b9adbfff058a6b001eef3855551581e73925200e2e5683ceb29439863ed82",
      "graph_file_node": true
    },
    "frontend/src/features/operations/chart-settings.ts": {
      "sha256": "88998e5aa44acab370281ec9eb58ed3111b86dfcbf542d05d08e1de7c4ddd9fe",
      "graph_file_node": true
    },
    "frontend/src/features/operations/data.ts": {
      "sha256": "8896796205484f051f790ae55ef1c39f19e82279f21edba7cf654c862a251903",
      "graph_file_node": true
    },
    "frontend/src/features/operations/index.tsx": {
      "sha256": "159f9d884ea72796481364d8ab7a5aaba2500962d442a2cad24ded198f5602a7",
      "graph_file_node": true
    },
    "frontend/src/features/operations/model-analytics.tsx": {
      "sha256": "1a48be654af7618113f2a25fdaf43af53b5b8d111b66cbcb3e3733570b75e6f4",
      "graph_file_node": true
    },
    "frontend/src/features/permission-demo/index.tsx": {
      "sha256": "768bda99c4a45d89b1a169f711a102260dac098e38dc7f32aebbb10438362b58",
      "graph_file_node": true
    },
    "frontend/src/features/playground/index.tsx": {
      "sha256": "ca4f2c6f856030135dbd84b6549e75c8feb93afbb70158381aaeea88edbef5ee",
      "graph_file_node": true
    },
    "frontend/src/features/product-experience/context.ts": {
      "sha256": "09bc2ad0f36d4f29164ad583dff04deddb4a4d99d3c68be2a9a011f3351d3d16",
      "graph_file_node": true
    },
    "frontend/src/features/product-experience/data.ts": {
      "sha256": "601eddcd1ed1194b9dc97d4e69443d047029374accdfe9bf23d6662363e94f62",
      "graph_file_node": true
    },
    "frontend/src/features/product-experience/index.ts": {
      "sha256": "4f702f31abd78b79f41f3302421e49db32384a4360c3b5f05b2a66d8720ee603",
      "graph_file_node": true
    },
    "frontend/src/features/product-experience/mode.test.mjs": {
      "sha256": "c0d9ad6128b4c2e289b37111a67cd76ad2d59da339f80fb6235255648135c890",
      "graph_file_node": true
    },
    "frontend/src/features/product-experience/mode.ts": {
      "sha256": "6647c79f4d4b0ab28e34fe9098cd08b2885eb5283ad228344ddd16f40b0a3b2a",
      "graph_file_node": true
    },
    "frontend/src/features/product-experience/provider.tsx": {
      "sha256": "f88abc69cc1662d1cd474935ebea94618e62dc9688fff92c3aa7d53db9c17241",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/data-table-pagination.tsx": {
      "sha256": "bc5419e007949a6dc1b1633696eba1fafd69721abbf6af07d55d11e2a026e4ff",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/data-table-row-actions.tsx": {
      "sha256": "838b238ef254f4ced8778b27128fd069a1028ed6cba37517159800fac001d390",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/data-table-toolbar.tsx": {
      "sha256": "b51a9dea384f027cd46cdfaf039b3233c6f993f7b0c6c1a77b4c404d507077fd",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/data-table-view-options.tsx": {
      "sha256": "1253b6d8217fc471a379aba9c9f4953b03266197fd7c59a6c703094e822a8924",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/project-user-action-dialog.tsx": {
      "sha256": "0cf295c0ada150f58c465e6f769f75ced3a290d697ab0f0dc852555f207b8b4a",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/users-columns.tsx": {
      "sha256": "ea50f210c101e61c49ae97441b224d639d1344ea430b66d2e288702aca700b10",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/users-delete-dialog.tsx": {
      "sha256": "d5dfe124e651a970158a6df5cc73e5acc50b5d50e4ea5318a62d3b9b69937b7e",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/users-dialogs.tsx": {
      "sha256": "bd6b2053ecd0265a7ae7372b92dd808056b1a4d22a234643fc05621e9a9f1852",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/users-primary-buttons.tsx": {
      "sha256": "be462ba36923325767154df1f4cf0f56055e5dc1b5c2b7dbf1867659d5e57323",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/components/users-table.tsx": {
      "sha256": "b6fd9f67f321fc9ac5eb89c8197ae89ff406d1c8a054dd9e01af93758596c22a",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/context/users-context.tsx": {
      "sha256": "8bed9bd8ad7e5475ae5e84808c8c1c93fbe9b1ebc95e8140484d95e169a259fe",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/data/data.ts": {
      "sha256": "e934967a18a794d2af93f4079304a45f524b862b94871b4e901e0ed82555e71b",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/data/schema.ts": {
      "sha256": "b4df50208068f02b18cffba52d867d32465bb1bc9d6b86dd9d850a20c430abcf",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/data/users.ts": {
      "sha256": "f89174f1850802bffe3ddad539b400920d12edfc55742f2788bc47ae06362ea3",
      "graph_file_node": true
    },
    "frontend/src/features/proejct-users/index.tsx": {
      "sha256": "7c9ee00b690b1833fe7930bdeeece5f7a670fa3f2bdf1453cb9da81b59ae707b",
      "graph_file_node": true
    },
    "frontend/src/features/project-dashboard/health-data.ts": {
      "sha256": "a682c39ddffd97f3692c37d288c2691ff8108d3dfdca9bf2596fff53d1f5164f",
      "graph_file_node": true
    },
    "frontend/src/features/project-dashboard/index.tsx": {
      "sha256": "f5f510f4c51462f9f0a3a083d60f2d7cff097609bbdc2a157fbea3ccacfea044",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/data-table-pagination.tsx": {
      "sha256": "bc5419e007949a6dc1b1633696eba1fafd69721abbf6af07d55d11e2a026e4ff",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/data-table-row-actions.tsx": {
      "sha256": "7350b07ede0c0a09a2c2c4cc680cb6ae74debaaa38bfc8ab1083f1679db3bd76",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/data-table-toolbar.tsx": {
      "sha256": "40cc14eb4a308127d15737473744e78653db2d126a6e19bd6f28fc8520025817",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/roles-action-dialog.tsx": {
      "sha256": "d325395867beaf1b0b427faf3d2036587d188c31ab3e43c9f6b9f9d70d62860e",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/roles-columns.tsx": {
      "sha256": "130ca8cf747f6113e4da31de8331058b7e84608fa26de048b395c54de95f7627",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/roles-primary-buttons.tsx": {
      "sha256": "9912324b95424fe547f2477c3b794a2573adfdc90962d54f31430a10b5ebec64",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/roles-table.tsx": {
      "sha256": "07ab48a97a6da06a9b3121a8eb7cb13125e5058cc7f59c43288ec9db13269aba",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/components/scopes-cell.tsx": {
      "sha256": "132e85347af248af54fde70bb36fe001c175cd87d56b7e0489d46de98bf53d5b",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/context/roles-context.tsx": {
      "sha256": "0882005025f3584d18e6c45b80509883acde6fa58dc900cfc5fb3cdb84616497",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/data/roles.ts": {
      "sha256": "ec556bbacf6bf5fa6af1518798ca10eb8198c860955e56d5b474f03cdf8eebca",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/data/schema.ts": {
      "sha256": "37070bd912ddc8b494abefde1850b5f97c41f0d736044b9e8a2f08b7db682acd",
      "graph_file_node": true
    },
    "frontend/src/features/project-roles/index.tsx": {
      "sha256": "87c5138b86ea5a7364b8544904bfec084ae373548038b216e8b47e1b63797dc7",
      "graph_file_node": true
    },
    "frontend/src/features/projects/components/data-table-row-actions.tsx": {
      "sha256": "7bce46bac37deeeb3158b2b601d2c3ecb4fff859463b802fc18591811268a04e",
      "graph_file_node": true
    },
    "frontend/src/features/projects/components/data-table-toolbar.tsx": {
      "sha256": "a8b4d6d610522f6525fbfcf7d9b3ed676c4f6805abf6fe2dcd7f9065e19cc32f",
      "graph_file_node": true
    },
    "frontend/src/features/projects/components/project-profiles-dialog.tsx": {
      "sha256": "f71d56a6eb48eb4a3c3adf3ece7309946b1b3128d319b2d1a552a6f4f868b1c3",
      "graph_file_node": true
    },
    "frontend/src/features/projects/components/projects-action-dialog.tsx": {
      "sha256": "3226cbd2b1ecd6f30754851c2fe68bdc6fd93a997295fb3eb68bf3a6e54428c7",
      "graph_file_node": true
    },
    "frontend/src/features/projects/components/projects-columns.tsx": {
      "sha256": "6f74fa7a2867c644300e844bc3054c7ff0171a56745083c916593b41d98ce4ef",
      "graph_file_node": true
    },
    "frontend/src/features/projects/components/projects-primary-buttons.tsx": {
      "sha256": "0a7a39d81a05092c0946cdba16819909c40cf48568c8aaa505249cf7b80d292f",
      "graph_file_node": true
    },
    "frontend/src/features/projects/components/projects-table.tsx": {
      "sha256": "b8e3b0f0ade7ca4065c21c7719efad2b97632b33ee15e62d0a290427b781bf16",
      "graph_file_node": true
    },
    "frontend/src/features/projects/context/projects-context.tsx": {
      "sha256": "cf869c4622beb86a59f38757e8b2d390aad3e09ac4b6ab9d22d55ef78a4b7b82",
      "graph_file_node": true
    },
    "frontend/src/features/projects/data/projects.ts": {
      "sha256": "4c2d3a62f1c2e69524b6ce7c004e322f2c85349a136291a456b9b27b8a39f95b",
      "graph_file_node": true
    },
    "frontend/src/features/projects/data/schema.ts": {
      "sha256": "5780aa8a6ae36b2e8360d44703c69646e93efd3f8ac7f8b0d6e660cb703320dc",
      "graph_file_node": true
    },
    "frontend/src/features/projects/index.tsx": {
      "sha256": "831d4f453393af45c0315dab426e50901f5389c8cd33e77f99e1efcf4ff5589e",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/data-table-row-actions.tsx": {
      "sha256": "1d9cd2fd124f7f20873bde6b4c79166660a6ffddd01fc20e5272458de9659fa1",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-action-dialog.tsx": {
      "sha256": "fd768e6b5fb0afa2eff14ce23254fe68cd14db8a39a8ca1efe36855f4cff2899",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-bulk-delete-dialog.tsx": {
      "sha256": "68e9dd0916de97ef58b29501ccdea4c670d436068d7c5dc7d6aac3b4b29b4b74",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-bulk-disable-dialog.tsx": {
      "sha256": "884d287c18103c01e9eb31386f9e71d9bac0d1abd1ab7c04b2c317f5535e9b1b",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-bulk-enable-dialog.tsx": {
      "sha256": "cd51c604af12f2b238b9809d3ae8e0b8933239e4262515c5d7f5999cc75be4e5",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-columns.tsx": {
      "sha256": "90261a9a741b5627afd3bee5909890adbc46c879b8d5654717a4267a6b1d3bf4",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-delete-dialog.tsx": {
      "sha256": "d760ec4fba26668c455beb4467074017ce7f6acc5247fb68d9fe1265d0f49b95",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-dialogs.tsx": {
      "sha256": "a164abc4bb2d542c55327eec8911aed683814240d63c1d9bbd8865311e66ded1",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-status-dialog.tsx": {
      "sha256": "33a5a6376c9cb717060d9ecf3dab494d62523a12f645a4920bc37e20034e056b",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/components/rules-table.tsx": {
      "sha256": "76c2164045212b0de90b738f101e10dba8d562d30ca9848132d76ee9a356c30c",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/context/rules-context.tsx": {
      "sha256": "9ff59430392345f799bc724d9b46573b7e067fd6fd72f1a56b8b54fa27ea6086",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/data/rules.ts": {
      "sha256": "94072487d59af62fc8dd92c5ac98a8932328d52c7e3523e66885caf07c42e9a3",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/data/schema.ts": {
      "sha256": "17e02ea6fc503b089e93aedecd440344084482eaf8bab12a00ebd477801caac5",
      "graph_file_node": true
    },
    "frontend/src/features/prompt-protection-rules/index.tsx": {
      "sha256": "45e8a2acf22d925b1e6a566c53e2230c64aae9df49af0ea70ab4fe6c834fb573",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/data-table-row-actions.tsx": {
      "sha256": "751487fe6b1d78ffc7f51770906f4468b3aea13f1bed237608854c710f5d99ef",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-action-dialog.tsx": {
      "sha256": "84d021670d1082a971c26d0d7910d76d3ad31a4550f83caf4d897dbf493be738",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-bulk-delete-dialog.tsx": {
      "sha256": "5fb0c931e392e0f0184a36753a7b79542324ccb96fa691a85794c24a21ec1a19",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-bulk-disable-dialog.tsx": {
      "sha256": "bbd3c3c8be0436653f6154279dc8b7800337aadd20cb629472e6c53497efc309",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-bulk-enable-dialog.tsx": {
      "sha256": "77bbb5378ba45dd519c721d567103e33dea770c970a1228647d79c664d45036c",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-columns.tsx": {
      "sha256": "ffb7ee2bb11bfc7d52af3111ec3fc00083179798e62f3bf183420dabd6b65f40",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-delete-dialog.tsx": {
      "sha256": "594f8bea087d955b76ec6d39f4364b9ee223c0d59fbdac4755458fa3322cda79",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-dialogs.tsx": {
      "sha256": "8e0e779bbb7ca68d20dbaac6e92eea463631b83ce071b188b25c7a2c3fe481d6",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-status-dialog.tsx": {
      "sha256": "208381964be76fda1035e8b941e2d0bfb015cfc687fc0efd2bdc0c4004291eb0",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/components/prompts-table.tsx": {
      "sha256": "33db733626e027860ec882a4686741a78be0b6257712b5e914d866ab419a315b",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/context/prompts-context.tsx": {
      "sha256": "86c81b777a409b8b88a216a8fc2f5025b1438bd0bd4302dfee1f4e38ef2afb29",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/data/prompts.ts": {
      "sha256": "2d004d358b2515ebc9b2f016f7363fb53fd03423af5804a7606c986bd1824784",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/data/schema.ts": {
      "sha256": "df49d24f952701e792d10aa58e6a0f1e82575837fed0bdbcc39c9e3227fe1900",
      "graph_file_node": true
    },
    "frontend/src/features/prompts/index.tsx": {
      "sha256": "eb061ba5224a57c660142674318d641f4ab11f476bd824c2d23f25ee0ce6db1a",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/chunk-item.tsx": {
      "sha256": "2f7cac540b8b981ec1f81d969aeb8d242811ce207ac2020cf943c384926dc080",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/chunks-dialog.tsx": {
      "sha256": "1220620cf4647cd684a25f8e1249ba61f0df3b4441a71d0a0f8e05f4df0ee8db",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/curl-preview-dialog.tsx": {
      "sha256": "f066ca39325f56c48771d2207220908fc64dd2fbe895d763e73874d9ebb3c011",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/data-table-pagination.tsx": {
      "sha256": "bc5419e007949a6dc1b1633696eba1fafd69721abbf6af07d55d11e2a026e4ff",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/data-table-toolbar.tsx": {
      "sha256": "c0109b19a7ca069bddb2678609755ab5de6ea80bb6b786bdf8a7a81c0b476fb4",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/data-table-view-options.tsx": {
      "sha256": "8dfceb2ffa1971955064101ca0b281720b9d902601032596f90fd469ca7d72b4",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/help.ts": {
      "sha256": "186db3894768c36ff98713bc9aa8c6c3957739847fc3ade84b95f029f1ff8466",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/index.ts": {
      "sha256": "1afad3e2e8a2aa53aed68e48edfe11df10f26b1ace0ce5ee4b0d763c7f79e179",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/request-body-drawer.tsx": {
      "sha256": "2e0d0f06e3a937f754116efc1f95e874d5ab179e1d03f0ac7ba35c04866db611",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/request-detail-content.tsx": {
      "sha256": "f806bfcde345537bbc95306a18edbc5114b5890dc0500ee69dadbca1bb0b704f",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/request-detail-global-page.tsx": {
      "sha256": "d5b94471ecb6edd3a39844b58f2d2d6b7022392949998b20c37f8dba4cb70f45",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/request-detail-page.tsx": {
      "sha256": "9a0ee14c58e784d6ce4a17b9eeb013a114e3aa0463044e473455f0ad4ca6a0d0",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/requests-columns.tsx": {
      "sha256": "3f131cb092b401ef5224ad47854a62ee0bb4071347bd23453c2234ed4b2db9b3",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/requests-table.tsx": {
      "sha256": "004de7f2a106be00ec7bff8912f0a2e247dc7494f31383aaefa8e78d6ec1a4b2",
      "graph_file_node": true
    },
    "frontend/src/features/requests/components/response-flow.tsx": {
      "sha256": "8598fd2124549d30d89fc29a393d9ba9c0189d4be0f78f48e0013a7ed21987ef",
      "graph_file_node": true
    },
    "frontend/src/features/requests/context/index.ts": {
      "sha256": "45acbe612e3074c7f6da7f5af0997e92caaf23f51ec00c88ff1150850c716bd5",
      "graph_file_node": true
    },
    "frontend/src/features/requests/context/requests-context.tsx": {
      "sha256": "d16035a4e9a38daae19de0bd4db335235c252eb08bcaf613066f48c1de308785",
      "graph_file_node": true
    },
    "frontend/src/features/requests/data/index.ts": {
      "sha256": "2665dfb7b5106d1b423a6464136f7fcf26a05e9f8424fda1b72c23abf0a2212e",
      "graph_file_node": true
    },
    "frontend/src/features/requests/data/requests.ts": {
      "sha256": "d0e7e78c2bd49dae121ed8d691656052122b992783ea3df6f3029214eba55a80",
      "graph_file_node": true
    },
    "frontend/src/features/requests/data/schema.ts": {
      "sha256": "dfb8645208aa6e797884e7844058681cb6e5b3828885b36e9a3df20ff3f0411b",
      "graph_file_node": true
    },
    "frontend/src/features/requests/data/usage-logs-schema.ts": {
      "sha256": "89d946e644c6bfea429286c1ef72b24cc26907e0741a9b44db194f4f9cb20561",
      "graph_file_node": true
    },
    "frontend/src/features/requests/data/usage-logs.ts": {
      "sha256": "ee3a436cd7369376450ed347b3070989f46053c9a66979d1c175419da797cf2a",
      "graph_file_node": true
    },
    "frontend/src/features/requests/index.ts": {
      "sha256": "1bc0b75ad18b6938bff5029b2c9fb94118d813e864464b3385df0403b321de3d",
      "graph_file_node": true
    },
    "frontend/src/features/requests/index.tsx": {
      "sha256": "0517dba562775650e66e00ab5759abba5bde0e316b733bba7f34903b12676da4",
      "graph_file_node": true
    },
    "frontend/src/features/requests/utils/curl-generator.ts": {
      "sha256": "d96e9e60e6e7c31d1342310ed1fb266084676fd05ad903620395c00b38292257",
      "graph_file_node": true
    },
    "frontend/src/features/requests/utils/response-parser.ts": {
      "sha256": "3a44e036c0dff9829944852dd03b4199cd985712fdc58ac8c71048f56345677f",
      "graph_file_node": true
    },
    "frontend/src/features/requests/utils/tokens-per-second.ts": {
      "sha256": "f87785edac7333f376354987a0edc497f41364d7e749657164dfb046155043f9",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/data-table-pagination.tsx": {
      "sha256": "bc5419e007949a6dc1b1633696eba1fafd69721abbf6af07d55d11e2a026e4ff",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/data-table-row-actions.tsx": {
      "sha256": "ab260858ee34b16745034896bb521edd2c6d9af777f409568494a1d7cddabe41",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/data-table-toolbar.tsx": {
      "sha256": "324cc53f6e47b5de9c6abe237a9cefc2dcec12c872b779c725d97e4e6c9c9f24",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/roles-action-dialog.tsx": {
      "sha256": "14632da919031fa907b1d9bb609c15547f9643eed186cbcb78bc456ca8116b36",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/roles-columns.tsx": {
      "sha256": "7a50e5c20a3e4f9138eefdfd2ea72f0f08c973ceb2040bf27522f1b37a904df5",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/roles-primary-buttons.tsx": {
      "sha256": "f1fb7d109977899fb7c88f5234371c4e4ddd4529714ea414d883c81d701e21a8",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/roles-table.tsx": {
      "sha256": "7673c31cdf04ccd2d2fc59d0d14daf5aa56337a1d94bf4be17c404b323746809",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/scopes-cell.tsx": {
      "sha256": "132e85347af248af54fde70bb36fe001c175cd87d56b7e0489d46de98bf53d5b",
      "graph_file_node": true
    },
    "frontend/src/features/roles/components/system-role-template-picker.tsx": {
      "sha256": "9b6bef0b852f4163763e81af8929261a0a67506b061e12d075fc866030c85877",
      "graph_file_node": true
    },
    "frontend/src/features/roles/context/roles-context.tsx": {
      "sha256": "f55e6004190fa48a68488f2fa39c1e7e595d65cc3f0a3da03efd4f402089392a",
      "graph_file_node": true
    },
    "frontend/src/features/roles/data/roles.ts": {
      "sha256": "aabb6c6d7cfed824c1e1873b4c1198107d621bee265cfe40230ff13984d09699",
      "graph_file_node": true
    },
    "frontend/src/features/roles/data/schema.ts": {
      "sha256": "20cb3335086b5c61fd9f24db5eb195328847e49165903fdf08024af8c8ab606c",
      "graph_file_node": true
    },
    "frontend/src/features/roles/data/templates.ts": {
      "sha256": "7983d72763dc9b5c7333332f85e6ce8162bf219fc9a9f88d4649ab7dba3f13c6",
      "graph_file_node": true
    },
    "frontend/src/features/roles/index.tsx": {
      "sha256": "8f0bd171c029b550bcc1fa26da18455178327288713b2a54f264822bf0dee972",
      "graph_file_node": true
    },
    "frontend/src/features/settings/appearance/appearance-form.tsx": {
      "sha256": "e351f96e205fb638b3bf81c8448bc1f7dfef7d3adbba4106205e3a870a3b5f7d",
      "graph_file_node": true
    },
    "frontend/src/features/settings/appearance/index.tsx": {
      "sha256": "0eaf76521b5eb656f4b386bed3f631e8c12a06660a35869337f9f77290f7a353",
      "graph_file_node": true
    },
    "frontend/src/features/settings/components/content-section.tsx": {
      "sha256": "35a91f36ee2f1503f7d2a646f4d34135811b13e65c90f7bff188c0f991d4a28e",
      "graph_file_node": true
    },
    "frontend/src/features/settings/components/sidebar-nav.tsx": {
      "sha256": "752a0828e0f904228b53fb69706ca10346ff2bb833349a44948b36ac02d750ef",
      "graph_file_node": true
    },
    "frontend/src/features/settings/display/display-form.tsx": {
      "sha256": "1affe08008127afa9fbf6425e8a6c2691b04f062710a69100f7926ea927b0d68",
      "graph_file_node": true
    },
    "frontend/src/features/settings/display/index.tsx": {
      "sha256": "33a66b653c0bf74b7e053762f5755e99c7bda71a09cca627ab657c6822c8058d",
      "graph_file_node": true
    },
    "frontend/src/features/settings/index.tsx": {
      "sha256": "046596ae78687d9846ee3b59791a7febf08682cac7a1a3f92c4eb2393bda3dc2",
      "graph_file_node": true
    },
    "frontend/src/features/settings/notifications/index.tsx": {
      "sha256": "e846337d892876881236b54241044ae07739e5463fc25e2b5b1b72c9b5297985",
      "graph_file_node": true
    },
    "frontend/src/features/settings/notifications/notifications-form.tsx": {
      "sha256": "f6a94679bd1fcfed8ba3ee2d0a57c114b59606f81f2531dc351db1bba15f3bdc",
      "graph_file_node": true
    },
    "frontend/src/features/settings/profile/index.tsx": {
      "sha256": "ca47b405aad505d173cdce6ee06f20e3fcae874f9a08f18711f0977437b9d9bd",
      "graph_file_node": true
    },
    "frontend/src/features/settings/profile/profile-form.tsx": {
      "sha256": "0ae7150a1b63bf79aa150116f7d3eaea9c1aa589b561cfe6511707945dee25cd",
      "graph_file_node": true
    },
    "frontend/src/features/settings/security/oidc-management.tsx": {
      "sha256": "fab3ab7245be1d0ee36b2000b31148bd9ac2304186bd68c69e16c5cf0f563188",
      "graph_file_node": true
    },
    "frontend/src/features/settings/security/security-form.tsx": {
      "sha256": "4c9aaa092f7066898b521ff9cc7288cfebf0c59800f928f033366f8554a3b44f",
      "graph_file_node": true
    },
    "frontend/src/features/system/accounting-ui-contract.test.mjs": {
      "sha256": "6d7955864b20280a901e9b86f3a9decf9630f2bab60a3c2a93b22913d50ce4c7",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/about-settings.tsx": {
      "sha256": "149ed34ab1639523e6b2d4fd44e0651340243d2966df6f6762bd524cfe5c11f9",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/backup-settings.tsx": {
      "sha256": "db51ef1ec09e3a3fd69a404cb6d5f86f276324639024b9bba332de9c9e36e660",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/brand-settings.tsx": {
      "sha256": "8a5dfa5201b020c8d11868f3567fdb920515d4eaeba9831e4e35dd8fb19f040f",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/diagnostics-settings.tsx": {
      "sha256": "415457b9c47aa6f6faa9334541d8f7c49bb9f43512563edd7674004ab453ec35",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/general-settings.tsx": {
      "sha256": "7c9739c27eb10c2cb1c58473bce4d045ed526af7712e41da906a0c8170970954",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/proxy-preset-edit-dialog.tsx": {
      "sha256": "506b8342cd9878b584e9a5345f7241301a73968084420828be43a7fc703929a9",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/proxy-presets-settings.tsx": {
      "sha256": "f5774e9a28984fccd84a9924bd03eb4013548b7bf2be2d56e024be5767ce1214",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/quota-settings.tsx": {
      "sha256": "cdee3ec49d9d0eb5a49c84415ef51ac49ca9b189a1b4ae97ece91fa9dc4aa21f",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/retry-settings.tsx": {
      "sha256": "37819b828379327865689b2cc69893f1001341b7ac805a32608ebb8687be56b4",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/security-settings.tsx": {
      "sha256": "a1c7cc70a26381e817fffaea62e7c0c0ca3506c560f0d64f0657eeac529e193d",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/storage-policy-settings.tsx": {
      "sha256": "77f6174bbd27c145a2348f00355196ea3e96d3f66c4cfe45e5879e2ab382e951",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/storage-settings.tsx": {
      "sha256": "36d802db809c712dc65709a13deed174b115d82b7b37ba89c8475df9a7228aaf",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/system-settings.tsx": {
      "sha256": "2b0a0cda9620513cb6c844a81798db8941204a6301ee3ecb3750721f7a26ad06",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/tabs.tsx": {
      "sha256": "d66df92e99f05d35c7299973afd8d459ac9bc31b9e4c197b43011c5723b891ad",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/video-storage-settings.tsx": {
      "sha256": "7aab86b7ff80649856780bb754d969b30eff6c44a6fb035baed463fd58a32f2f",
      "graph_file_node": true
    },
    "frontend/src/features/system/components/webhook-settings.tsx": {
      "sha256": "2a2f31671d28f3206e24ad08084021f9dad04e0c1711ffa0b4843367e5108659",
      "graph_file_node": true
    },
    "frontend/src/features/system/context/system-context.tsx": {
      "sha256": "04a820312668cbe1d00739e77cfab72ff8b7088c6036014f60d621b3292d7398",
      "graph_file_node": true
    },
    "frontend/src/features/system/data/currencies.test.mjs": {
      "sha256": "1dd87c184fd7567a105ba5b9737031d23a173e9c95e9979f78aa512ed1aa466d",
      "graph_file_node": true
    },
    "frontend/src/features/system/data/currencies.ts": {
      "sha256": "74a0a6561b65f468a1fe0e1cf11271fa5baf5a78b73163f9faeeb4b83e3c51f6",
      "graph_file_node": true
    },
    "frontend/src/features/system/data/quotas.ts": {
      "sha256": "90de8c98053bdeb1b8cbe1a1ea6cf654fff6184b77af81b322d10bf75fffeb3e",
      "graph_file_node": true
    },
    "frontend/src/features/system/data/system.ts": {
      "sha256": "314b87a18a9cd6d13b618a17ddac5e7096b90c12815ec17d4461f4af05d64684",
      "graph_file_node": true
    },
    "frontend/src/features/system/data/timezones.ts": {
      "sha256": "00c8bd3ef04f4064d3122cb2ae480ed4cd40406ea4da594dd985f288d9957b15",
      "graph_file_node": true
    },
    "frontend/src/features/system/index.tsx": {
      "sha256": "3b212199b7f6d95498e5a745852115b4ab7804a3cae96a3ef63b5b89e18b54ab",
      "graph_file_node": true
    },
    "frontend/src/features/system/version-check-disabled.test.mjs": {
      "sha256": "da131891409d6bbe95790854c755bea4e9d786ee60410e039c85bdd411a7bcb1",
      "graph_file_node": true
    },
    "frontend/src/features/threads/components/data-table-toolbar.tsx": {
      "sha256": "c6a6b23ca96d3b63a8d33a3226e0b91be3e7e66fceb0e137c0ebbdfe1c4bee4b",
      "graph_file_node": true
    },
    "frontend/src/features/threads/components/index.ts": {
      "sha256": "47d6dec7e4259b467be5bdc1eefb91ed87abbc6161c8b1890ed3b07acff65160",
      "graph_file_node": true
    },
    "frontend/src/features/threads/components/thread-detail-page.tsx": {
      "sha256": "10ef6d61209dedf0561d1d542c39d33d0a0a10a669c9548aa80ece5eac42fda3",
      "graph_file_node": true
    },
    "frontend/src/features/threads/components/threads-columns.tsx": {
      "sha256": "2c3b670c3abe2bcda47d8e81d5ab2ad6d93a04398106b6da4a84d07254303897",
      "graph_file_node": true
    },
    "frontend/src/features/threads/components/threads-table.tsx": {
      "sha256": "eaf726627d2998ee15de7e5c711e8d6eb2d013c320ee32a4dd95b057770906e9",
      "graph_file_node": true
    },
    "frontend/src/features/threads/components/trace-card.tsx": {
      "sha256": "8ae4ca1878e5037ebb9cfbdd2b2b8784db5fec2d2d1ba64a2d413846d2358340",
      "graph_file_node": true
    },
    "frontend/src/features/threads/components/trace-drawer.tsx": {
      "sha256": "10450abd02591ff8e359e96a2f9a746e8fc5f3143e2a7a051c4951dea740c57a",
      "graph_file_node": true
    },
    "frontend/src/features/threads/data/schema.ts": {
      "sha256": "36f9d9df72cd8c41e55fd7f4b6972c57658472b82ae6d05aa50fb2d9ec9429e6",
      "graph_file_node": true
    },
    "frontend/src/features/threads/data/threads.ts": {
      "sha256": "93397f4164a9577c98b3bb14b87045a2fd6226944572905deeef1d76ec9622b9",
      "graph_file_node": true
    },
    "frontend/src/features/threads/index.tsx": {
      "sha256": "5aabb7c4881c4743f3b6642c7d715c2102b86c2eb1e88f333ab1afe022577c91",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/constant.tsx": {
      "sha256": "082098fcbfed119b6cc660fdd72f235511ba9cfc647b20c6fe46395042682b03",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/data-table-toolbar.tsx": {
      "sha256": "5ac1234f1fd055b512863906ae96ea6d863be0f2ba1dfb59d17705577eaeba78",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/data-table-view-options.tsx": {
      "sha256": "1253b6d8217fc471a379aba9c9f4953b03266197fd7c59a6c703094e822a8924",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/index.ts": {
      "sha256": "2513a1d5fe202a4b23bedf2cf15f7b363647ea170fb3fbc135e8787700a691be",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/span-section.tsx": {
      "sha256": "75c47de28ae70d541c5871faa62b85fdcf084206fb882b16917c41a8b63ed9b2",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/trace-detail-page.tsx": {
      "sha256": "3927f95a234603013d48c65c824d8f09e12ad58f2b8e60e84c1a2f628ea22a84",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/trace-flat-timeline.tsx": {
      "sha256": "46f9402dba8cbcdde0a7206072cd88290961003b984fe6414f43df791f9313b7",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/trace-flow-timeline.tsx": {
      "sha256": "c6dfd03ec0faf4999d203fe1de9dc334fd8f6dfee18bad5b0b5bd5c565df5e43",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/trace-timeline.tsx": {
      "sha256": "b9f062f98a312f06e5a2d6d5ee411b2a510ed5948a9a4fa85f7cd59204819c41",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/trace-tree-view.tsx": {
      "sha256": "311946f9c27f4c10c6a54533c8777755ee822eafd2814aefb74ee2d0a99f774b",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/traces-columns.tsx": {
      "sha256": "f4744d1b70712f3e816d0bae21468d746fe693531cf33f0f1334aa36dcd88e08",
      "graph_file_node": true
    },
    "frontend/src/features/traces/components/traces-table.tsx": {
      "sha256": "1b76d38ef7207a9a65499a1907d50b765f6f37aa5a42acd6b44485fcdd20a29f",
      "graph_file_node": true
    },
    "frontend/src/features/traces/context/index.ts": {
      "sha256": "8adadb96bfd344ec4cb590982c2ce66e3767b66ee5590304b135f1746c5d32df",
      "graph_file_node": true
    },
    "frontend/src/features/traces/context/traces-context.tsx": {
      "sha256": "77f57bd0c9b4c45fa9a3b9ca37e1e18d73afe946d59fb14e2d8a030f330513e7",
      "graph_file_node": true
    },
    "frontend/src/features/traces/data/index.ts": {
      "sha256": "47ab02a31176433989c7709313705d1c46959597214931f9bf1234e8fe7ec14e",
      "graph_file_node": true
    },
    "frontend/src/features/traces/data/schema.ts": {
      "sha256": "ff4c56ba2ce8962a8c41504e690681bfc1016eb49dcfc424a60eeb5b7e627a9e",
      "graph_file_node": true
    },
    "frontend/src/features/traces/data/traces.ts": {
      "sha256": "92da505c41a456e3f40bfe17b44efe20859ed83290d00cf8a0d9dbc92543627b",
      "graph_file_node": true
    },
    "frontend/src/features/traces/index.ts": {
      "sha256": "63b31e2972a909b89d2983250c0417d3cbe8c273acd27a07f44365971016ce63",
      "graph_file_node": true
    },
    "frontend/src/features/traces/index.tsx": {
      "sha256": "2721c9436acfd8e25bc8331d97c096685a504b29de8ba7f274ed217427a67d4b",
      "graph_file_node": true
    },
    "frontend/src/features/traces/utils/span-display.ts": {
      "sha256": "316eede2692923bd439dd7b4ea959b8b967259983bc0a24d22f9837c554456fa",
      "graph_file_node": true
    },
    "frontend/src/features/user-groups/data.ts": {
      "sha256": "f9be0dc31e03a3a9c983fcce32ab45fb43c59da44976ff1ede7bd06e01ac79c7",
      "graph_file_node": true
    },
    "frontend/src/features/user-groups/index.tsx": {
      "sha256": "282b82435f936266ddeb899f69669044f00f3bbbe40dd24ee69b2d8e8d6bea35",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/data-table-pagination.tsx": {
      "sha256": "bc5419e007949a6dc1b1633696eba1fafd69721abbf6af07d55d11e2a026e4ff",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/data-table-row-actions.tsx": {
      "sha256": "c2c04ed18d1f0b6e1f6a47af2bfcb4b3c3256d11d92ba4f4eb74e12030e37c36",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/data-table-toolbar.tsx": {
      "sha256": "b51a9dea384f027cd46cdfaf039b3233c6f993f7b0c6c1a77b4c404d507077fd",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/data-table-view-options.tsx": {
      "sha256": "1253b6d8217fc471a379aba9c9f4953b03266197fd7c59a6c703094e822a8924",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-action-dialog.tsx": {
      "sha256": "e6fb393bf08f5159ec695c7fcaf034814728c8d0fbd082d82639874e484d1fad",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-add-to-project-dialog.tsx": {
      "sha256": "cccf09b9232094dacb41d8e62e8239650b5c92dd123d016bd320f7c82fda77b6",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-change-password-dialog.tsx": {
      "sha256": "1a5c931b2548d215917eeac86f580aada23d27a7e9b9008a20c2467fa5c11521",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-columns.tsx": {
      "sha256": "ec1f3267c8e4dabdc8ef9e1099864fd9ac9e7ff9411fb8ae3eb2b5498f1b9f74",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-delete-dialog.tsx": {
      "sha256": "4b4c8e953b1859450cf0e22ce710af91fdde6b8174d6d70d211d9bd943db169b",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-dialogs.tsx": {
      "sha256": "3a3f67accd3a979b8af3a253bccfdc768a42d579ba2e0be04fa0323d58139b43",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-invite-dialog.tsx": {
      "sha256": "a3a4f4966a3322e59955b6e648e441866ad2b0a728b72f79eedf36ade6f4be45",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-primary-buttons.tsx": {
      "sha256": "22b744542086297fa06a23432b7bf26c1ff1edf83ce3711570594f0ffccca81e",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-status-dialog.tsx": {
      "sha256": "6e1a4d94ddc30e8faaef30e1d45348b8f98272a05ccd0c994d24de5bc5f68505",
      "graph_file_node": true
    },
    "frontend/src/features/users/components/users-table.tsx": {
      "sha256": "bf6b24a52bfa15d28c483106081b8374aa390490f69ad5f3868cbfd5a7467b85",
      "graph_file_node": true
    },
    "frontend/src/features/users/context/users-context.tsx": {
      "sha256": "82e94e5c955dbe05541bde2a0d994952dedfacb02dfbb0932859974d4e55e464",
      "graph_file_node": true
    },
    "frontend/src/features/users/data/data.ts": {
      "sha256": "e934967a18a794d2af93f4079304a45f524b862b94871b4e901e0ed82555e71b",
      "graph_file_node": true
    },
    "frontend/src/features/users/data/save-error.test.mjs": {
      "sha256": "457a78f7b85b538f14e9f947897b0a5b8347e4f75fe11ffcfd5b57a85d6f3c9d",
      "graph_file_node": false
    },
    "frontend/src/features/users/data/save-error.ts": {
      "sha256": "9d76118100db954fe301f674a4c81ea7fab6f3f1a4eb35dbb8767b5721d93d2a",
      "graph_file_node": false
    },
    "frontend/src/features/users/data/schema.ts": {
      "sha256": "bab958954246d732efcd7957436e6227d0c5ae72589f5d7491796a755d89c388",
      "graph_file_node": true
    },
    "frontend/src/features/users/data/users.ts": {
      "sha256": "84b6f367f2c930b3f9e3e5fe0c38460a7257dfcf115d04a3e9c8c330648601b4",
      "graph_file_node": true
    },
    "frontend/src/features/users/index.tsx": {
      "sha256": "98dd0cf0cf8790929ea7ef65a25a9804efd72ad5c79e2641c98932097d5a1ead",
      "graph_file_node": true
    },
    "frontend/src/features/wallet/components/redeem-code-dialog.tsx": {
      "sha256": "c2caa6970b9864e65998c6cb302b15195fa29ac8cf7011183d73597424d15006",
      "graph_file_node": true
    },
    "frontend/src/features/wallet/index.tsx": {
      "sha256": "61f58a548cceb55174e6ac8ee7e066474f4ab7979193ea377faee4d72278dd06",
      "graph_file_node": true
    },
    "frontend/src/gql/graphql.test.mjs": {
      "sha256": "6d288d7e2ab74dee0d0e0103fbb152cd2786e05fc4c508f6d29cdf100122650f",
      "graph_file_node": true
    },
    "frontend/src/gql/graphql.ts": {
      "sha256": "1073b7749202e85a59b1a745178ae257706f67c11872af3ff982de78cfe68889",
      "graph_file_node": true
    },
    "frontend/src/gql/models.ts": {
      "sha256": "747dfc369756e736d7a18557269353015d83639c225b74e74f5d50411970cbe8",
      "graph_file_node": true
    },
    "frontend/src/gql/pagination.ts": {
      "sha256": "72cf5265b4b2211af763b9293ea98a7f62dfd42301b621e54ad4129c97d95951",
      "graph_file_node": true
    },
    "frontend/src/gql/roles.ts": {
      "sha256": "108be68a79731f34fb2cd3ab099ab8942448314b8273866ea0d0c5b93bfa4e68",
      "graph_file_node": true
    },
    "frontend/src/gql/scopes.ts": {
      "sha256": "c0af8b022f3c0fff3fcd638ff309e9d4ed4cd54bc74dad0d7c48f575aaf5da98",
      "graph_file_node": true
    },
    "frontend/src/gql/useUsageLogPermissions.ts": {
      "sha256": "ffe93e3d598e3ff903f66aac6ffd408e3aec0fd12d8ec7b55d315ebba235daa1",
      "graph_file_node": true
    },
    "frontend/src/gql/users.ts": {
      "sha256": "a83332d6a0e8b2620e40230d83bc3b47db14bc2391b90df60e5058cea4473819",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-admin-price-display.ts": {
      "sha256": "c9d2f9fae892a206b6b1b16308e3c69f75b3aed411853150943f301fac8af73c",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-auto-scroll.ts": {
      "sha256": "c383dc1de1baa72a5c954b3fbb63593190c5613672476203a8bb32ba1b6728df",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-autosize-textarea.ts": {
      "sha256": "45d1d4867857f968c5af8dccb4e3ae55166698d1ff37a84acece0516b7c7f1b8",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-click-outside.ts": {
      "sha256": "abad9134b484f73b0ac8a98b4ccae27ef287d86c39646326ed1e93e0e3eeece8",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-copy-to-clipboard.ts": {
      "sha256": "89c224bd091f004d70fea6fe88e086f789368c5af713a47db0d00bd158489dae",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-debounce.ts": {
      "sha256": "6618e94738740597b581d511e1ef5eb85b67666fe9380bacc9d4cf3c639a065a",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-dialog-state.tsx": {
      "sha256": "4a7ee33d799172bb3bf9ed883e000fbeaac34d0f7839b636fc1c90df8b9c14bd",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-error-handler.ts": {
      "sha256": "37513682a2c79d44dfa68ed500e99d537c39189ec937690e66ee29f77e98b173",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-mobile.tsx": {
      "sha256": "3438549e4f0638803d84037a6ae686b702706311a828bba57df9a067df43ea69",
      "graph_file_node": true
    },
    "frontend/src/hooks/use-pagination-search.ts": {
      "sha256": "2c80e02bdbb76f7274df488554071c4ca99f7365b13299c45906b063a7a15195",
      "graph_file_node": true
    },
    "frontend/src/hooks/useAnimatedList.ts": {
      "sha256": "d7b55be2119c2d84f218a2cc2c7c507c80d76d63e9fca2481661168e39fd9d20",
      "graph_file_node": true
    },
    "frontend/src/hooks/useInterval.ts": {
      "sha256": "2dd6651f195ea7a966a4c4f52cdcf4e83a8393fb405dae44f445e03d45f82772",
      "graph_file_node": true
    },
    "frontend/src/hooks/useLanguage.ts": {
      "sha256": "1fdc9130cf7e43bd7acd3d41b7a61c4912ead8e25b0ccc4b1f65e95b4cef073e",
      "graph_file_node": true
    },
    "frontend/src/hooks/usePermissions.ts": {
      "sha256": "958bd6c13a2e6cc8ae299b9897ff52704ef7ce81a70e978d8abd224507c2005a",
      "graph_file_node": true
    },
    "frontend/src/hooks/useRequestPermissions.ts": {
      "sha256": "fb94798f0bdbf5ec743a657ce9f2c6e4e967c6815dcc4115fbe0abfba3b91724",
      "graph_file_node": true
    },
    "frontend/src/hooks/useRoutePermissions.ts": {
      "sha256": "a189d821808eef3030dead7f46f92124a97511a7504b2ca0c0c524f62827a719",
      "graph_file_node": true
    },
    "frontend/src/icons/index.ts": {
      "sha256": "a13e40225c0d1fef69362b642235fb2801cc439c2d11c7b3bfffeeb67a1f96a3",
      "graph_file_node": true
    },
    "frontend/src/icons/thread.tsx": {
      "sha256": "8831add4bf44d5208e464e17057531de2e17adf625f4f7f0171bccdb2187a9aa",
      "graph_file_node": true
    },
    "frontend/src/icons/trace.tsx": {
      "sha256": "84c67e42c4d6f8a2070d7e8f0ceefb0a3f7bdc70c86a97667cf05fdf25676d5c",
      "graph_file_node": true
    },
    "frontend/src/index.css": {
      "sha256": "934689e7c3f13c46ed6824c138f553487d775e8d089bd970b912ffc1098f7b00",
      "graph_file_node": true
    },
    "frontend/src/lib/accounting.test.mjs": {
      "sha256": "fbfa50b2d24f8aac6b7865b2f96641fdefdc1600fc47013b41105718fbc79803",
      "graph_file_node": true
    },
    "frontend/src/lib/accounting.ts": {
      "sha256": "2075104ca7b92b0873a182175233b680b6104d472aa147780c05b1d503ecad62",
      "graph_file_node": true
    },
    "frontend/src/lib/api-client.ts": {
      "sha256": "6bf8245108133d5e2aead33e60a821f5a08d55fdfb1eca5e3ba84867872d97ce",
      "graph_file_node": true
    },
    "frontend/src/lib/audio-utils.ts": {
      "sha256": "88abbc5fc097cdb4adac76bcff566ee8848e6335d99b9377d59ec7b5e8e1cc29",
      "graph_file_node": true
    },
    "frontend/src/lib/base-path.test.mjs": {
      "sha256": "3bb9ba89de4f7f97a84ae3351a46162a455ae5379df9876480ef2b8114676dd3",
      "graph_file_node": true
    },
    "frontend/src/lib/base-path.ts": {
      "sha256": "360feb2c25298f2e15cd4aff29ca263dda2de64f9884f334b49e653cb5e91193",
      "graph_file_node": true
    },
    "frontend/src/lib/currency-format.test.mjs": {
      "sha256": "0c0fd0c66993702740f6b1a503d6e85c723883c41f1d170697f1f543a2a352c4",
      "graph_file_node": true
    },
    "frontend/src/lib/currency-format.ts": {
      "sha256": "3c6e0f1b74ab8200840c68f6e2c422d285d1db3afdbbf5290358c82df749fe1c",
      "graph_file_node": true
    },
    "frontend/src/lib/error-parser.ts": {
      "sha256": "6c60f1afdc157e4c82b395a31d985058eb6f852136b621276813f2281635ebfc",
      "graph_file_node": true
    },
    "frontend/src/lib/i18n.ts": {
      "sha256": "5943491bb0590ba7da3d2f7cc5d94ed09630117dce84eaa16b21c7de1cd9a239",
      "graph_file_node": true
    },
    "frontend/src/lib/lobe-icons.ts": {
      "sha256": "8091294b5528226346f1f0125a21705ab5dc319d92299db398a36efcbf3a403b",
      "graph_file_node": true
    },
    "frontend/src/lib/permission-utils.ts": {
      "sha256": "c997d4d9cc2a35c681e8667c15071d5825f0180a864c0a6a64b7afd32d2d1b19",
      "graph_file_node": true
    },
    "frontend/src/lib/utils.ts": {
      "sha256": "6616e7045ca12d340e9711b72f8a2d2f0b710e71fc5bbc4ead094fd0361ea5c3",
      "graph_file_node": true
    },
    "frontend/src/lib/validation.ts": {
      "sha256": "390c7a6caf80431a13821267b6e3e586d34edf24f304757c5f25a9cf04921596",
      "graph_file_node": true
    },
    "frontend/src/locales/check_translation_keys.sh": {
      "sha256": "53a9069a93b83c01bd2b771f80c9c7cc939843020a350bbce72d0523b9298b19",
      "graph_file_node": true
    },
    "frontend/src/locales/check_translations.sh": {
      "sha256": "8f5032f57d9c276309f4ad873c26e92f1bf1a7802a944a4acf7cd50e9cab87c4",
      "graph_file_node": true
    },
    "frontend/src/locales/cleanup_common_columns.sh": {
      "sha256": "195afbea9a1562cac3664676ec44ee34c85ba192af62bca68f90b77f5b0379cc",
      "graph_file_node": true
    },
    "frontend/src/locales/en/apikeys.json": {
      "sha256": "1c622879571d0681bef5aa5c985c113fe75e076bc9ea2f5f89a75a44b4f9b6a0",
      "graph_file_node": true
    },
    "frontend/src/locales/en/base.json": {
      "sha256": "a14e2a7a6c4aa5eb8c7f3caa2a78ccb8a8a941a6e2d41bdda10b19ad6ff51e75",
      "graph_file_node": true
    },
    "frontend/src/locales/en/billing.json": {
      "sha256": "215fa2da16bad92c4d2e9eac236af8f8f1bbf4a9b24bae9d1c8c5dbbe01528db",
      "graph_file_node": true
    },
    "frontend/src/locales/en/changeSets.json": {
      "sha256": "169d2ebe70234cc04b437d4c53ad1bace2de2fa68107276dc691871581f1e772",
      "graph_file_node": true
    },
    "frontend/src/locales/en/channels.json": {
      "sha256": "b41876ce36c27a4a2dcf7e76a50d073522d215cd459b7d3d371330b68f11b55c",
      "graph_file_node": true
    },
    "frontend/src/locales/en/currencies.json": {
      "sha256": "b92b101f388e5b54831e99884e8f13c8e6955a437e3c25c97ecb34270bb27efd",
      "graph_file_node": true
    },
    "frontend/src/locales/en/dashboard.json": {
      "sha256": "9391b4c778c5ad940c21bc52a5b38b37e82e0ed7640559a75c7a7003ba294410",
      "graph_file_node": true
    },
    "frontend/src/locales/en/dataStorages.json": {
      "sha256": "b0b8c30df14d361784ea862d8bb74afa7f92d46b26bd02404cc685645dc6bb1e",
      "graph_file_node": true
    },
    "frontend/src/locales/en/financial-onboarding.json": {
      "sha256": "5718f73677950ffda509c49cd412d7da4a6242b2220ff53db5d051a71ea7f747",
      "graph_file_node": true
    },
    "frontend/src/locales/en/model-market.json": {
      "sha256": "2277003d9c9e3691771ea30634025a002330acc140f09be8dec352ad7315c4a9",
      "graph_file_node": true
    },
    "frontend/src/locales/en/models.json": {
      "sha256": "06fff2faf0547ab0a3cd8f41c4d6317fcb21ab5071aabe00726b80860fc95ee9",
      "graph_file_node": true
    },
    "frontend/src/locales/en/operations.json": {
      "sha256": "805d275594c4b515da8d6105202494dabb7b2771694ac021763090f9adc5523a",
      "graph_file_node": true
    },
    "frontend/src/locales/en/playground.json": {
      "sha256": "0c49d9742108bf5251d1f434bfe7f39b7229553da821b0720e8fe35b7d5395bd",
      "graph_file_node": true
    },
    "frontend/src/locales/en/profile.json": {
      "sha256": "3f6efcee1a2985571f14c9a5f67d3c9040554d169cb6727ef5d1a5d5b9c3d040",
      "graph_file_node": true
    },
    "frontend/src/locales/en/projects.json": {
      "sha256": "90a1b0c37ebe28f3d93bdc4deaafa563406dcb4f80efa2d1936b947a3cb07ffe",
      "graph_file_node": true
    },
    "frontend/src/locales/en/promptProtectionRules.json": {
      "sha256": "f44dee4bf30f9757329191b39fddd0cbf17340f3a08881a38024d672b70f6c6f",
      "graph_file_node": true
    },
    "frontend/src/locales/en/prompts.json": {
      "sha256": "8f777cb41559f3bb0e5240dd2eff29a46a4d47c7d3cc07c519ac066b94479a20",
      "graph_file_node": true
    },
    "frontend/src/locales/en/requests.json": {
      "sha256": "4cf89c82c893f2231716e5f7ede356d9f7a75eb0e46328aa62e7b8ed757e33fd",
      "graph_file_node": true
    },
    "frontend/src/locales/en/roles.json": {
      "sha256": "7f1439324f66d3101f8a37a8e5f326c202499c5ad4b1c96318f1f7bf5d1c43db",
      "graph_file_node": true
    },
    "frontend/src/locales/en/settings.json": {
      "sha256": "e433d14c88905c061ab21ebd60fd7bfe5b580e205c1d89fd5e8fc91c0c066726",
      "graph_file_node": false
    },
    "frontend/src/locales/en/system.json": {
      "sha256": "50f5fad83f1e8e4080b1c2f7649c759312d15f264707b857cc8fbf135dafc891",
      "graph_file_node": true
    },
    "frontend/src/locales/en/threads.json": {
      "sha256": "83155c0afe313faef3670142c7fc0e3348cd7ece9e81e940a3d892adb280693b",
      "graph_file_node": true
    },
    "frontend/src/locales/en/traces.json": {
      "sha256": "6073bb61c87d0437d548ec230e3bda0ec242612bcadc4058d2fa183b0534b4d5",
      "graph_file_node": true
    },
    "frontend/src/locales/en/usageLogs.json": {
      "sha256": "0b01700550bdbe79ecc40d6abbe2a528d179259dddefc1ff34207ac6586dcf9d",
      "graph_file_node": true
    },
    "frontend/src/locales/en/users.json": {
      "sha256": "7e11a8ed84747367ea256816554800ccaf97057a18302ad00c9c6d6796412c57",
      "graph_file_node": true
    },
    "frontend/src/locales/flatten_i18n.js": {
      "sha256": "3f53be09f82ca85d54f56c8ca7379232f027c27ad8d13e71300e5d452284cd6b",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/apikeys.json": {
      "sha256": "a0abea31f7fd1a509e7900ffdb92cdcdc6940c88f63b264b396e80df10518177",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/base.json": {
      "sha256": "64072e2a6a760a85dc5e7aadd0b101cce97124effabc86c401758006507fcc50",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/billing.json": {
      "sha256": "efc1f6f7cbddeef3700c31c8f62afadb0706a1eb0143a9d4dd6a59a2ee829792",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/changeSets.json": {
      "sha256": "99f3a5ce1cd084a2f024c3247c1937fd31661737ec67261bb885e844e10c23bc",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/channels.json": {
      "sha256": "4ae5969ac20c4fad5ea008c406737220629374a2751918040375c20651a12db8",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/currencies.json": {
      "sha256": "cd24b127005d26e3218d2464c330df3de13d3ea8ad6116357a73d0eb84cbb5bb",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/dashboard.json": {
      "sha256": "eb4c4d69dbe63c7c2609383c13e874ae4b450a595de654b87c6898d6434a5e38",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/dataStorages.json": {
      "sha256": "013f0c41ffa7931569164451204083fff16b1daad51220b5a63eaaa7bfc9fcac",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/financial-onboarding.json": {
      "sha256": "f4ef63716554b175ffdf9a0c192cfa9da0f2cc734cd707091eb6d67abce2fbc6",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/model-market.json": {
      "sha256": "eac5e5ecadda75659d8e32050eb7598f50f6203faf86321c80f38fb026d982d5",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/models.json": {
      "sha256": "ce04e8e52e8396556c59f6f32de5c8d307fa3204526957c57734432eb93c9b0f",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/operations.json": {
      "sha256": "e458befd0689ce9e95091de086fdf85425e1247e9d7ab75b71f1951cf8a0c1a9",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/playground.json": {
      "sha256": "c79df0ee20cf602aafa70f0cf12e4708dbb18f28e7cda03cda0afd0d61fb29af",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/profile.json": {
      "sha256": "58aa76fe6e278669848db5745c0617fc38716301e62edf5ac8e59ede7995d514",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/projects.json": {
      "sha256": "8ce9106c4e4ea941c9ea77628e3609a456a9e76ebec1debc5ffcd41049fe360e",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/promptProtectionRules.json": {
      "sha256": "831e9845bcaaca5d9e1f361c2864b2793842803b9fe7fa88b77821c26dd25366",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/prompts.json": {
      "sha256": "254ed0e19aca83443c3bda793f597ebb19bf688127c89781a0242194edb7a940",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/requests.json": {
      "sha256": "e7725aee6eabf70026659f93ec60647b3f29e226aadd4cae958fa61a765d896a",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/roles.json": {
      "sha256": "4c4a4ab9f34e28b74c5d1374bab50d0a719d7e2ea1f12c44cf4f71f066d9bdd3",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/settings.json": {
      "sha256": "4a1ac5e8321eec1b6d64aaaaf0e342e12f656b7e9f9c7f438252cfca0715e9d2",
      "graph_file_node": false
    },
    "frontend/src/locales/zh-CN/system.json": {
      "sha256": "a5c708af9755832e971fb1491774f5a0c7925da6ef4c5acbceccc2f97aa192ac",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/threads.json": {
      "sha256": "17e4b6f2384ef77a5da6881ac1332c48e36af27c9866904a37a4569761c4a33f",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/traces.json": {
      "sha256": "3de82062d7c30d7aa1eeb886a19a09764e7b07df70c82f34130f2dd7e55d1f9e",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/usageLogs.json": {
      "sha256": "5e394d18f24361f0cd9c586b0481f7375eb890aa5a6097a0e60d1b5d0269566c",
      "graph_file_node": true
    },
    "frontend/src/locales/zh-CN/users.json": {
      "sha256": "792538d3db9eb150e2f2fec17006bad19265254185194daf428d27ec33cc8ad2",
      "graph_file_node": true
    },
    "frontend/src/main.tsx": {
      "sha256": "a0e69ac73296e229a061e0e45b6863d58c845e2da42ed0f5dec1cb0906eb60ad",
      "graph_file_node": true
    },
    "frontend/src/routeTree.gen.ts": {
      "sha256": "1ad60d2a6ef52a39d72f432927f551a2f9d92e97775bcdf768db5730f3cb5d95",
      "graph_file_node": true
    },
    "frontend/src/routes/(auth)/forgot-password.tsx": {
      "sha256": "52fef79ce5aba3708caacaed66fa9098e1b4cbe6e0eef1f51c65cd7717c1511a",
      "graph_file_node": true
    },
    "frontend/src/routes/(auth)/initialization.tsx": {
      "sha256": "ef718672759e0b1f25030436ed0b94bde9e8d602f4ab74050a02b583ce7c8408",
      "graph_file_node": true
    },
    "frontend/src/routes/(auth)/sign-in.tsx": {
      "sha256": "e671036b75598156e182ae012ac6308b1e4779a43d9235eb31fdd58ab9fb01da",
      "graph_file_node": true
    },
    "frontend/src/routes/(auth)/sign-up.tsx": {
      "sha256": "df94536cdb9f63d2072ac5b690fb4aef8ece46e313e12c449f6b64b9d4ea5c5f",
      "graph_file_node": true
    },
    "frontend/src/routes/(errors)/401.tsx": {
      "sha256": "6bba8d1f41c5557942f33c5342625a02f616641ed2d8b2c989a8fb40e48c32f7",
      "graph_file_node": true
    },
    "frontend/src/routes/(errors)/403.tsx": {
      "sha256": "808eb2a463a2618a75c80e1aab17f6002b03a884d9a8cc060da905c39cd81ce7",
      "graph_file_node": true
    },
    "frontend/src/routes/(errors)/404.tsx": {
      "sha256": "5be8b3c25f4af07e394263beaff0b0bd7e7e74144ba1ffb45c9b88a562983fbc",
      "graph_file_node": true
    },
    "frontend/src/routes/(errors)/500.tsx": {
      "sha256": "f4d6c27d2e67d24bfe83a10064e5119e33b0a779082c123af9e14635bfa9e347",
      "graph_file_node": true
    },
    "frontend/src/routes/(errors)/503.tsx": {
      "sha256": "e770d6912a3fbe7159a701d2f8c2e75da4f7fca87ff9bcf1a8ce6b6149fefe98",
      "graph_file_node": true
    },
    "frontend/src/routes/__root.tsx": {
      "sha256": "78f35f8c813691f9ba3f0afd5fd925995d7626cab2b887abab3a13e90cb22aa2",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/api-keys/index.tsx": {
      "sha256": "b05da8bbd9564036555aef67f95caa4ae47393597202f874a2fb30bb195f739f",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/billing/index.tsx": {
      "sha256": "3d2f48c631cb8652f55808b77b14b146c9c88a0a45de4c2b81512d625140f27e",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/change-sets/index.tsx": {
      "sha256": "9efe1f44b16a0d5717b73b7f0c48e095ab2be8305299742bebcdf3e77b5575f5",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/changelog/index.tsx": {
      "sha256": "55a306dfc350b206aecc02506788a1e8d7d645588d47926d9b0c3a788b1a9fae",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/channels/index.tsx": {
      "sha256": "c8474723ef018ad847f93dd4196ea950b17c7a919a51f0e26cd0c7175b216b8e",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/chats/index.tsx": {
      "sha256": "2f7a3aad1142fa423b9e457c519277abb1525194960754dac2c646d96c81c119",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/dashboard/channel-success-rates.tsx": {
      "sha256": "bac33281b8b69d8f73cad03cf0bb384e2c66cff213b47c1ecc47d16fd49d579e",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/data-storages/index.tsx": {
      "sha256": "89a3b4e15868f17d35a0b9a69d2a36cbf39e3d2512b303df9bbe9d1f1a1a6866",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/groups/index.tsx": {
      "sha256": "ec431443ba4dcd83048f2520944400f1e45af1ad7820ff089c305da4136225a6",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/help-center/index.tsx": {
      "sha256": "9c0a122a506780ea8f420d5000c14bddaab0fccb01d5c60b82b9668e5f62d68e",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/index.tsx": {
      "sha256": "4bc229f34c955d00e40362d7cfa1b01c878a0181300b4ca9b43e06c1bc2df833",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/models/index.tsx": {
      "sha256": "cc1a15ca529dca7aead4223cbab37e5d4a4792f8137170ac52f1c3daaf9c1ceb",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/operations/index.tsx": {
      "sha256": "50098d9583e1186b02474bd2b4ef2f1e22d7c750b79ea395ece36a05eb8cd67d",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/permission-demo/index.tsx": {
      "sha256": "9d82ece0093eada5526baa5f33a19d95f10177fb6ed61b78bd9de7b5bef503e7",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/permission.tsx": {
      "sha256": "07516f23a7df3a6404eec60929f81bbe695e3857f28b4a19ef4e38dad2ae3ce2",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/api-keys/index.tsx": {
      "sha256": "f1d8b86b0fb896c074b7d8dd87d9a6edfbf8b0653a775a737d8bd8f1321cfcf5",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/dashboard/index.tsx": {
      "sha256": "ca23aee702b22b6b6a5b687d861870a10c6318315028a11aab6dcfc959aebd36",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/models/index.tsx": {
      "sha256": "6ac0569288368141bff16166898259cc7cbd7c9f8d093ede6a2cc2fe510d7ada",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/playground/index.tsx": {
      "sha256": "dd7a29bb4e5f0f7f8c6a215fe10b092495c19f726bd24a9f121311fb982ad3f1",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/prompts/index.tsx": {
      "sha256": "5f36efc831546075164e1ceacc687dcbec616243c8de2dbcb2efdd8681db55c2",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/requests/$requestId.tsx": {
      "sha256": "df0c039680332529330cd022feb47d5d4569155cc55e47ed5f54de8b96a3575d",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/requests/index.tsx": {
      "sha256": "00b0a6c6dfcee669edf040839098e672f7055c9094ad41645568b115310878c9",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/roles/index.tsx": {
      "sha256": "600ee656bbccbc4553acb396f8cb40ee27430f2e5ad09020710f0a328e664069",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/threads/$threadId.tsx": {
      "sha256": "cd9758592be23f3bdce48a307115f600d793f4886d5a6afa8ea0cf8217e023fd",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/threads/index.tsx": {
      "sha256": "e8a66be04bf7daf6960e2ab55a1349c3a5db7007b4423b7193824f9c0a5a589c",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/traces/$traceId.tsx": {
      "sha256": "1b29ecb1fd500e29efa82363f01cbd3ca253c6c230bfe629f722bfb120215c08",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/traces/index.tsx": {
      "sha256": "88a71acb4bab5be6f49c627e4879b4b3229c9c6589c920d001a37797bdf925c3",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/users/index.tsx": {
      "sha256": "9dec56410a0adbbd4a44bc392328f18c3f61218c2881d4befe1f9436537578c8",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/project/wallet/index.tsx": {
      "sha256": "1109805cb7ce366eb36f0c9e342ba3b666f12be9a826063b8681b5838fa63e14",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/projects/index.tsx": {
      "sha256": "af0a2e8cf43ae75fcb29fba552dd73f32aa3cbb1a6aa1f62bca349cbc972824a",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/prompt-protection-rules/index.tsx": {
      "sha256": "18759b344cf53126ecbd098685734dda24e6d5f2ee9247eb455bdd29b4ca063e",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/requests/$requestId.tsx": {
      "sha256": "8f0ff383a591ed448b2cbe442acd0ea74b5fbd721898ef74902b43fe678a1dd0",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/roles/index.tsx": {
      "sha256": "4961d6ed6c263b0297456ff8376f59181104f79fa8efba96443d18fecf7bb115",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/route.tsx": {
      "sha256": "4e7b1858312df02f8e62ae9a39fea14f774ae7a2d5fb1d7fb876de805a58f96b",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/settings/appearance.tsx": {
      "sha256": "6e9c71e5728454c73ba8cf48d9e6dd7fb4bd5a384ecb6a9e107b7843c87ad9b6",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/settings/display.tsx": {
      "sha256": "c893e9e986b9a3c55895b3e6654b378b3039619bad4f2f553eff0e7f9ace7472",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/settings/index.tsx": {
      "sha256": "48d3642b39972fc436cc4f413801e49bdeade50932e4fcd80f0dda256b100d26",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/settings/notifications.tsx": {
      "sha256": "626fd6bfb7731e6bd5c8f0b545fd3223056bf6771cc9de4575345dd697762c54",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/settings/profile.tsx": {
      "sha256": "ed391e725f2496875402c396411a0996425886ce89951de00f3c13488866581e",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/settings/route.tsx": {
      "sha256": "79dd4811a11793c1420cde321c418bfe632579c6f739a6832a256e535aa601c7",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/system/index.tsx": {
      "sha256": "7f6a33042ea8823fde4ac45692f4f9659b11c5c584b9a632500613f65288550a",
      "graph_file_node": true
    },
    "frontend/src/routes/_authenticated/users/index.tsx": {
      "sha256": "6ab1576a434284e60c5dccc915c360f968ab3dd1c4ebca0bb7f5a1e310812707",
      "graph_file_node": true
    },
    "frontend/src/routes/oauth/oidc/idp-callback.tsx": {
      "sha256": "0762626ac154a9d58bef0f7353ac605342f5b003920409f7a90c97ee75379bff",
      "graph_file_node": true
    },
    "frontend/src/sidebar.ts": {
      "sha256": "fa61593585b9df5f543ac7c6c7133f1c6bb01c314ae9623f11a9d5998c7d1523",
      "graph_file_node": true
    },
    "frontend/src/stores/auth-storage.test.mjs": {
      "sha256": "7571674ac7f00bf57ba3e6a936eff1172fda3394d95583d1de093a3f89381ad4",
      "graph_file_node": false
    },
    "frontend/src/stores/auth-storage.ts": {
      "sha256": "83df6382e7b0cdbe0ea4b5ddb03ea0a8f35721701cf3a934a95cd734fb098624",
      "graph_file_node": false
    },
    "frontend/src/stores/authStore.ts": {
      "sha256": "a18669a3591f9961c112d40e8eac263689018d3e04c1358581a697d56368268a",
      "graph_file_node": true
    },
    "frontend/src/stores/pricingDisplayStore.ts": {
      "sha256": "3cad7730bc23ffc5ec03263c911a3b5609945eb6fee88576dc6c09a14a5fdfa6",
      "graph_file_node": true
    },
    "frontend/src/stores/projectStore.ts": {
      "sha256": "3ebd5dc1e2f80a27cd1678c4852925a28f12257bc2d4274ff045771a5e7572a4",
      "graph_file_node": true
    },
    "frontend/src/utils/date-range.ts": {
      "sha256": "5c0eebea9b27b8dfb959c78a10debf40ab0ec149d704e3272ed23a490797ad54",
      "graph_file_node": true
    },
    "frontend/src/utils/format-duration.ts": {
      "sha256": "2b66eec2a39cc1dfa018ebe3cfd56ae475e0bc33e37872fface3b901ffe8b934",
      "graph_file_node": true
    },
    "frontend/src/utils/format-number.ts": {
      "sha256": "5294d5cd508f865ba14cd65652bf6c02ba521bf07c679ee8b310c62d4e7410ae",
      "graph_file_node": true
    },
    "frontend/src/utils/handle-server-error.ts": {
      "sha256": "9209ce7ce981c6da3d1e52f64a05e3eb1f6df0af1926f154d8afbb80fa6dcd53",
      "graph_file_node": true
    },
    "frontend/src/utils/show-submitted-data.tsx": {
      "sha256": "17a9b866de3f5e2675b053ee1082c81585f4de7c8ef5894ec88827317f7fd767",
      "graph_file_node": true
    },
    "frontend/src/vite-env.d.ts": {
      "sha256": "65996936fbb042915f7b74a200fcdde7e410f32a669b1ab9597cfaa4b0faddb5",
      "graph_file_node": true
    },
    "frontend/tests/api-keys.spec.ts": {
      "sha256": "4f5a49557e006949c3e01251cab4fdcaa2b944c835ac2d72413294c9c865dbc1",
      "graph_file_node": true
    },
    "frontend/tests/auth.utils.ts": {
      "sha256": "42fb0d6416c9457578e49a71c111524bb9a84e71c23c60e8fb933413a6237306",
      "graph_file_node": true
    },
    "frontend/tests/channels.spec.ts": {
      "sha256": "f001c81abce1b4d284f412fd7349a426db025cce8a72fbff04eebe040b40d7bd",
      "graph_file_node": true
    },
    "frontend/tests/copilot-device-flow.spec.ts": {
      "sha256": "10f8a09801193d4c9d8d5f70e7773c8d77e60f1f8e747037fe1d6951169e5823",
      "graph_file_node": true
    },
    "frontend/tests/data-storages.spec.ts": {
      "sha256": "0b203fc66a444cdefc7ae4f0ea6dee0700bc6dcedb63d8a86728545557c4d8a4",
      "graph_file_node": true
    },
    "frontend/tests/mobile-layout.spec.ts": {
      "sha256": "cdb28609decbb15f5ab24ec5b80c01dedabd7cfe41e1fec8a86d150c091d8190",
      "graph_file_node": true
    },
    "frontend/tests/models.spec.ts": {
      "sha256": "476912b4998b11e9ec5b7115beebdecd2df81f7654338de834fab5e183e9cdbd",
      "graph_file_node": true
    },
    "frontend/tests/project-roles.spec.ts": {
      "sha256": "c491c1af7dd4c90bc775a7f6057fa268392432282d796f1d3567d56aae3d6996",
      "graph_file_node": true
    },
    "frontend/tests/project-users.spec.ts": {
      "sha256": "a77c994576bc730d0e533f5fdf217259c776a47b82644454b4981a10785e1a88",
      "graph_file_node": true
    },
    "frontend/tests/projects.spec.ts": {
      "sha256": "ea7589cb1e9852c3d1b727dfd69d69e25fb0864295ca590f6dd9d7d6cc5fdaa9",
      "graph_file_node": true
    },
    "frontend/tests/requests.spec.ts": {
      "sha256": "fff94db09abe47e2c0bfffdb8ad35a5377a27fbcb4534756995ddc4372ab6e19",
      "graph_file_node": true
    },
    "frontend/tests/roles.spec.ts": {
      "sha256": "61acbe6626fdda535623eb10268dc71d794448a123dae619b638b8635d42d7cf",
      "graph_file_node": true
    },
    "frontend/tests/setup.spec.ts": {
      "sha256": "140a1da05cea72366accecd83d4b78396e8f5cbd254c22e4a1e4729b4dd350e3",
      "graph_file_node": true
    },
    "frontend/tests/system.spec.ts": {
      "sha256": "0e3b311aaf4fd00099ea7d2b73f2dae28058bf4dea3217601f18ff0cd2c5d745",
      "graph_file_node": true
    },
    "frontend/tests/users-projects.spec.ts": {
      "sha256": "b77930f041c56f45a62c435b444b6e7244d55c7a35aaaac06cb1265859def915",
      "graph_file_node": true
    },
    "frontend/tests/users-roles.spec.ts": {
      "sha256": "fc71bdb035b78c3be74d204d26133868911e3f038ebe35bb2d7e0f4cbee0ca08",
      "graph_file_node": true
    },
    "frontend/tests/users.spec.ts": {
      "sha256": "d5829fffcebc511abc91dab2467a378e81e451b1b7a29089f467c95d0844fdb1",
      "graph_file_node": true
    },
    "frontend/tsconfig.app.json": {
      "sha256": "b667cf9aa47d9321e1a894dcb339642c5c5ca1e559813c2ca1543edb61297ce6",
      "graph_file_node": true
    },
    "frontend/tsconfig.json": {
      "sha256": "3727519fb7bc2436eea9fa91a34e05e77e89036122e1e84b9b72bb0040055c81",
      "graph_file_node": false
    },
    "frontend/tsconfig.node.json": {
      "sha256": "cdf03296db009821b4da76adbc99712072f85fa48731d5c0b4e7f8d28a82f453",
      "graph_file_node": true
    },
    "frontend/vite.config.ts": {
      "sha256": "cc91cf0386eeaf037c884a3fc743ec8f8f5dcb98cab3bb5d6956e784d9ec63ef",
      "graph_file_node": true
    },
    "migrations/postgres/000001_initial.sql": {
      "sha256": "943779a268b571a77b4263e590bd21a7e72101ed69eeaa098002280f3fcbfbf0",
      "graph_file_node": true
    },
    "migrations/postgres/000002_commercialization_v2.sql": {
      "sha256": "617c888a3a7487c8f8f32f1478fc901dc0cf7394f9c905b7b8713425fe49796f",
      "graph_file_node": true
    },
    "migrations/postgres/000003_balance_subscription_v1.sql": {
      "sha256": "39fabe72e1e1d299300c7336c1f5f04d7a0da2b84247403903c08c8c67beb9ab",
      "graph_file_node": true
    },
    "migrations/postgres/000004_subscription_entitlements.sql": {
      "sha256": "804d296f1b5465c8caed67eee1f76a5fd6ba86a63fa539905ff6bf0ad88066e5",
      "graph_file_node": true
    },
    "migrations/postgres/000005_project_commercialization_v3.sql": {
      "sha256": "b6e919037244f86ea41afe0a75a709adccfd4b37d010515d40c762cedbcd5a97",
      "graph_file_node": true
    },
    "migrations/postgres/000006_subscription_project_grants.sql": {
      "sha256": "be4f4b3b3041ce270493f4df3c5ba6a8c9c762d1c3a05a2e9ff94464d7f9b88f",
      "graph_file_node": true
    },
    "migrations/postgres/000007_project_wallet_shadow.sql": {
      "sha256": "2c5b5e3fddb4bcc4891d57fc7feabfa000b24d9af39ba2e917789ae5d8cfce39",
      "graph_file_node": true
    },
    "migrations/postgres/000008_project_wallet_shadow_lifecycle.sql": {
      "sha256": "3222ad1729be39d0f218cf21c2d42bdc761f3c20cf4ca8fe5e5e23c5b9fda6b5",
      "graph_file_node": true
    },
    "migrations/postgres/000009_commercial_operation_audit.sql": {
      "sha256": "10d1e7139a7318d09fc78f9d2af2d2a985ac0660f5e462c8b8ebd6905f335c79",
      "graph_file_node": true
    },
    "migrations/postgres/000011_simple_groups.sql": {
      "sha256": "8d506fe5d4490f6d70059fb5825a0e71299c60764ddcb047c5acd76e6a6eb9e1",
      "graph_file_node": true
    },
    "migrations/postgres/000012_simple_group_membership.sql": {
      "sha256": "984d18ffd4857cb20a33dd1f34283d861ed146ddd460da8ce29d424173141560",
      "graph_file_node": true
    },
    "migrations/postgres/000015_channel_quota_snapshot.sql": {
      "sha256": "71113bbba331cf734b2b95714d14faec59bf9f8a3f2cd245c0205db8f5934a5c",
      "graph_file_node": true
    },
    "migrations/postgres/000016_provider_quota_probe_verification.sql": {
      "sha256": "6d47c547d8974af084b206781ef2add7730a14804c20d93c27393fb0bbf28ca3",
      "graph_file_node": true
    },
    "migrations/postgres/000017_provider_observations.sql": {
      "sha256": "e69cc93ffb1ac012ab806d2c6dd48f003ed2c982bd23ead1ca92f93b0ae70adb",
      "graph_file_node": true
    },
    "migrations/postgres/000018_api_key_quota_admissions.sql": {
      "sha256": "0f03aa11bbbf357bd09acbe71489a6e85ccd487b8c0b5a4832799b99498d1cda",
      "graph_file_node": true
    },
    "migrations/postgres/000019_subscription_assignment_snapshots.sql": {
      "sha256": "298a100de0f3d067f2628fa26b5ac65e00b4ae9a45997f96eb553f33c1d7f000",
      "graph_file_node": true
    },
    "migrations/postgres/000020_subscription_plan_snapshots.sql": {
      "sha256": "804d296f1b5465c8caed67eee1f76a5fd6ba86a63fa539905ff6bf0ad88066e5",
      "graph_file_node": true
    },
    "migrations/postgres/000021_request_route_explanations.sql": {
      "sha256": "07e0c9e151c1a98d67cffa5d8851011bd2725606e6df18abb4c63d1674e26d11",
      "graph_file_node": true
    },
    "migrations/postgres/000022_request_execution_credential_identity.sql": {
      "sha256": "abd1c9347ef06b199e8348331cc48f9b6be25b8119b4daf765d3d2f881c0569b",
      "graph_file_node": true
    },
    "migrations/postgres/000023_postgres_performance_indexes.sql": {
      "sha256": "656a73bfeff7153a45ca8d871134a01cccd2220fd1e537c16f6ed94ffa6d5480",
      "graph_file_node": true
    },
    "migrations/postgres/000024_remove_redundant_request_index.sql": {
      "sha256": "3e8159e0cbac8b0cd50d48b75f40569a260e53dd9049e4d82f4724058a415e8b",
      "graph_file_node": true
    },
    "migrations/postgres/000025_remove_unhelpful_usage_indexes.sql": {
      "sha256": "8ba62290d8f274cad841540efe95afe1fa1c21dd0c297bc93441712bb44e2be1",
      "graph_file_node": true
    },
    "migrations/postgres/000026_project_wallet_balance_snapshots.sql": {
      "sha256": "64cb09fd42fc562fec66fc07ab7326235921b37814b1b109061ac084d6bece55",
      "graph_file_node": true
    },
    "migrations/postgres/000027_usage_charge_outbox.sql": {
      "sha256": "cb7a0b36572e47eee0ff6e415013e268ea93afecc98fb9645cd60906e4e778a0",
      "graph_file_node": true
    },
    "migrations/postgres/000028_accounting_currency_station_credit.sql": {
      "sha256": "dbdf9d302b448ea33dd7d0f571ae40156d65d4583003f1aa98a344534ca5f350",
      "graph_file_node": true
    },
    "migrations/postgres/000029_route_affinities.sql": {
      "sha256": "f3bfff2301e0cae95fa95fae9a2c85d63abc9819a2d9a766cb5c623c9855afae",
      "graph_file_node": true
    },
    "migrations/postgres/000030_provider_price_accounting.sql": {
      "sha256": "225557e02580fffd1f4ed88aa4646b5560c4016c42267f735c02cf2f98a660da",
      "graph_file_node": true
    },
    "migrations/postgres/000031_pricing_change_audits.sql": {
      "sha256": "b88ca6a3416a39782d3ab78e853f4f231ffd1895c5d0b573e15439206e58ca0e",
      "graph_file_node": true
    },
    "migrations/postgres/000032_change_sets.sql": {
      "sha256": "3cbcbd43dfa251c60d04077906dec10de8e0408482a63fd19cb6c3ddf727ee2f",
      "graph_file_node": true
    },
    "migrations/postgres/000033_credit_redemptions.sql": {
      "sha256": "7986e2367f7d961e4861f9fb6bf538071ffa8810562e87cde207751ec5271217",
      "graph_file_node": true
    },
    "migrations/postgres/000034_credit_redemption_limits.sql": {
      "sha256": "a00bf059c2ce73bd9f06644ba01faad52a93cd28eae179f1d2820dd315b8e1c0",
      "graph_file_node": true
    },
    "migrations/postgres/000035_api_key_concurrency_leases.sql": {
      "sha256": "2ff8b5b13ad201cf110963f39bcd6ef108499a178b18c1dc1aae9f2f328999fa",
      "graph_file_node": true
    },
    "migrations/postgres/000036_recovery_lifecycle.sql": {
      "sha256": "955e567235a21ce4f0b434c94003e3914303da369f40c2ea85c4d536a560fa54",
      "graph_file_node": false
    },
    "rust-toolchain.toml": {
      "sha256": "fb0a56ca2fc09db10159f6983e9d747f4afeb64e1d6d941dc36a3c20bf3a5749",
      "graph_file_node": true
    },
    "scripts/db/set_mock_route_mode.ps1": {
      "sha256": "a1feb8f81e8d7def0d08c2036b944b2b72385ca23dce9bd6cdefbe4cf2b07074",
      "graph_file_node": true
    },
    "scripts/db/verify_migrations_layout.sh": {
      "sha256": "99654cf63add76111a695043e297c7d103bdf10685a245412baa81efa5c1d03f",
      "graph_file_node": true
    },
    "scripts/e2e/e2e-test.mjs": {
      "sha256": "afe8cad201d3b5e7f9f0f950220b4e243529a0bf8baaa8c9d61e34859caeaa60",
      "graph_file_node": true
    },
    "scripts/generate_config_schema.sh": {
      "sha256": "cdc5c653a49aac8fea5b730e975a969ee675081e5def38493cbdd312cd606f35",
      "graph_file_node": true
    },
    "scripts/knowledge.py": {
      "sha256": "356a1c07aa4f3728010501489b94a5eaf0ea4cdac9515c588529cef0583abb7c",
      "graph_file_node": true
    },
    "scripts/licenses/check-rust-third-party.sh": {
      "sha256": "7d8034529efe0f7942ca6d2a614d82733ad33c02ced1ea8b85554251c45db38d",
      "graph_file_node": true
    },
    "scripts/licenses/rust-third-party.hbs": {
      "sha256": "dd5cb8cf633793ffa361f70d0ee21aab1cf08cbe8fcca27900f9decc2dcdc9e3",
      "graph_file_node": false
    },
    "scripts/provider-compatibility/provider-smoke.mjs": {
      "sha256": "fffdacbd4379deed083ca5912a491f02904d8e822c79185e0db45fd5b826406c",
      "graph_file_node": true
    },
    "scripts/provider-compatibility/provider-smoke.test.mjs": {
      "sha256": "ff91e349a162eb95711559ea64a42676505a3a320328c0e6d4a024f71b86f523",
      "graph_file_node": true
    },
    "scripts/rust/security-static-check.mjs": {
      "sha256": "0cdc559952ceede6db63c2daa68d21f9c613b558c1b3b2d211f2e5c3cd78690d",
      "graph_file_node": true
    },
    "scripts/rust/smoke.sh": {
      "sha256": "8ce2157275393ada6e32d4da8fc3ed19c62b39e7157aa880eb47b5cc73fa97e3",
      "graph_file_node": true
    },
    "tests/contracts/admin_graphql_schema.graphql": {
      "sha256": "5d7d1bc4ee45ecd8a5356c22ac2f774371b6ca5661b20e8b7aac933547b81808",
      "graph_file_node": true
    },
    "tests/contracts/config_defaults.json": {
      "sha256": "9127e3be1ab7c291223e61a99f6071b1f834f7ff63b944b6090de79d0486a280",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/aisdk/request_system_message_conversion.json": {
      "sha256": "5a62e8912c5e61ceb5a2589b10e51547c7188c70b78a21a3a3cd7a5d5f6a7ff3",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/aisdk/stream_reasoning_then_text_aggregated.json": {
      "sha256": "c0734824e175361f02c7e9b408c1b8d5733524032cb17a0870aaa4ae8f01b2b6",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/aisdk/stream_text_deltas_finish.json": {
      "sha256": "7b071d9333ca58af08dbf6b568fd18153c17eebbc4f2d789a6359002744aadef",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/anthropic/error_inbound_internal.json": {
      "sha256": "6cd6ee5e3761c39f5701f0c8595b9e33cd2227f09ce48e6c63dd65a3c1b8dc0f",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/anthropic/simple_text_request_response.json": {
      "sha256": "21b5da2802989e510fa27a8a97de3f711e480312034bd1328b19d269dcfd887b",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/anthropic/stream_message_stop.json": {
      "sha256": "255bd87bd77d57214ca5818726de9fe5a8df5dc2ad77f8ec2c1871e0cd206279",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/anthropic/system_message_request.json": {
      "sha256": "02fe0df02c13f86ae22eca7f1e557399c0938965079361dbe2c5bc445464e528",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/anthropic/tool_calls_response.json": {
      "sha256": "1a3129063d515449b344a4691404ff51d5d057d7279fa2cd3bd2c32a96b9c974",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/doubao/chat_completion_basic.json": {
      "sha256": "91be989e65a9ec8dec63ca9f5b1b433e27166108b67db2376a82b0c2a6118595",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/doubao/metadata_user_id_request_id.json": {
      "sha256": "72c283262937ca112325935e0f00cbc5dc18d4dfb196d9552bb135632ff8572f",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/doubao/url_trailing_slash_normalized.json": {
      "sha256": "a167e51e3b8bc7d16690e6e09980907d90bebd1f0044a0b3391f86f06662c767",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/gemini/error_outbound_invalid_argument.json": {
      "sha256": "5ccef86a9bbe9677a75cdf923013a861d9fc2c801c01197824a274943ac639ed",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/gemini/modalities_image_request.json": {
      "sha256": "fa25f5513f192349ce705db4f715222bf29d5c094b3c986ab4fa95db5cdbd570",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/gemini/simple_text_request_response.json": {
      "sha256": "45e80b157fbc31f71cfab7174def01578df40ece798e1cedc1760c0c7f8dee34",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/gemini/system_instruction_request.json": {
      "sha256": "648e5dcd04251d16a9aa381f057275a725dec7f00d29d51382d2af8541d4e7c2",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/gemini/thinking_config_request_response.json": {
      "sha256": "ad3016c0e866969a62d06ef8001e914bad0164487ca79b9d20fb7e4222520ea9",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/jina/embedding_inbound_basic.json": {
      "sha256": "74579e92cb552ba13e342729b1771e74c12f6272d1d6017175363ad0fb63c625",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/jina/embedding_outbound_response.json": {
      "sha256": "4e2f0a066266bb3f3d7de5a1ddde2846e0cd9036e36baf070715444ab8c68094",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/jina/embedding_outbound_url_and_task.json": {
      "sha256": "aa841da984d95bed95394427b089d48815c671dac5890fb70d2d485f0274152a",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_audio/speech_round_trip_binary.json": {
      "sha256": "267800125eed5f7166dd0e0753227955a8e651d78f399bd0ef999b2073c5740c",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_audio/speech_stream_binary.json": {
      "sha256": "679b61272664d7e9ab064e848e79825ac997e2d799e00c50d5864437ad5e326e",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_audio/speech_stream_sse.json": {
      "sha256": "d3d053bc271784e72a644f7fb35bb551853d301fdcb87c780930ae8a7d12f065",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_audio/transcription_upstream_json_response.json": {
      "sha256": "3ff55d39398e74a2a844681cba2c6ead76aecd9b074f30f77b1b43e6f7b51d72",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_chat/basic_valid_request_response.json": {
      "sha256": "0fdb6f07da65f78730ba1bc068000e69dca31cfd5f3b7cfc54d045e3a5b33962",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_chat/error_outbound_invalid_request.json": {
      "sha256": "c96d310c7c8ce137a2301665fde967c6257fe0d107c93680736eba8acac37793",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_chat/outbound_url_trailing_slash.json": {
      "sha256": "a68171073e5c05bd3304801d2503841feaa0c25a552177c8966267de5a8863b3",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_chat/stream_text_delta.json": {
      "sha256": "e44edf7745774d116ad3b6e1d0532a312718e41977cec4290fa6340d54aa1143",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_chat/tool_filter_responses_custom_tool.json": {
      "sha256": "5aaef3e0263c547e5dc4085684c857d4bf95da1e1cb30008b88484406c111338",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_image/generation_client_response_data_url.json": {
      "sha256": "397eb784a99777f8e54813b74e6452c09fcbff012bdf86b50a6dea711f810aaa",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_image/generation_json_request.json": {
      "sha256": "39c3e6768ef1ca0358f96dd7c5d3658419174389929feb3998171de682d65043",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_image/generation_upstream_response_usage.json": {
      "sha256": "669c5c791b33589a48450f6147a1f6cefd3b08c7afca1a6670f1c05af2e69288",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_responses/chat_cross_protocol_client_response.json": {
      "sha256": "5dc59c35b42150fa9ee71ee94e2f0e701e51f0ea33ca953b4a9547c170426708",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_responses/compact_client_response_reasoning.json": {
      "sha256": "6a371a2181532b71e49d1d2a17523c7f3555b7da18cf984c51a819bfceda3f27",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_responses/compact_request_all_fields.json": {
      "sha256": "cbe3bddf506223cbe35fe5d3a8330971265ce9df6c218e9b342cd7aad03e0f6d",
      "graph_file_node": true
    },
    "tests/contracts/llm_cases/openai_responses/compact_upstream_response_reasoning.json": {
      "sha256": "67da5ce268f43a9b414c697e47ae028b1be1bc756b31850f9347f7e398508113",
      "graph_file_node": true
    },
    "tests/contracts/openapi_graphql_schema.graphql": {
      "sha256": "cad4595e5e64e22a6459435c6088712542b867f28b5923341598deb322d58513",
      "graph_file_node": true
    }
  }
}
```
