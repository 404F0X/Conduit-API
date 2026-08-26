param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("normal", "fault")]
    [string]$Mode,

    [string]$DatabaseUrl = $env:CONDUIT_DATABASE_URL
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($DatabaseUrl)) {
    throw "DatabaseUrl or CONDUIT_DATABASE_URL is required"
}

$common = @'
BEGIN;
LOCK TABLE channels, upstream_model_deployments, model_routes IN SHARE ROW EXCLUSIVE MODE;
SELECT set_config('conduit.route_mode', '__ROUTE_MODE__', true);

DO $block$
DECLARE
    primary_channel_id bigint;
    secondary_channel_id bigint;
    target_public_model_id bigint;
    primary_route_id bigint;
    current_deployment_id bigint;
    current_upstream_model_id text;
    current_variant text;
    canonical_deployment_id bigint;
    fault_deployment_id bigint;
    duplicate_deployment_id bigint;
BEGIN
    SELECT id INTO STRICT primary_channel_id
      FROM channels
     WHERE name = 'E2E NEW API Primary' AND deleted_at = 0;
    SELECT id INTO STRICT secondary_channel_id
      FROM channels
     WHERE name = 'E2E NEW API Secondary' AND deleted_at = 0;
    SELECT id INTO STRICT target_public_model_id
      FROM models
     WHERE model_id = 'mock-chat' AND deleted_at = 0;

    SELECT mr.id, mr.deployment_id, d.upstream_model_id, d.variant
      INTO STRICT primary_route_id, current_deployment_id, current_upstream_model_id, current_variant
      FROM model_routes mr
      JOIN upstream_model_deployments d ON d.id = mr.deployment_id
     WHERE mr.public_model_id = target_public_model_id
       AND d.channel_id = primary_channel_id;

    -- Repair the legacy fault state which overwrote the canonical deployment
    -- identity and caused the channel trigger to create a duplicate discovered row.
    IF current_upstream_model_id = 'mock-error-500' AND current_variant = '' THEN
        SELECT id INTO duplicate_deployment_id
          FROM upstream_model_deployments
         WHERE channel_id = primary_channel_id
           AND upstream_model_id = 'mock-chat'
           AND variant = ''
         ORDER BY CASE source WHEN 'manual' THEN 0 ELSE 1 END, id
         LIMIT 1;

        IF duplicate_deployment_id IS NOT NULL THEN
            IF EXISTS (SELECT 1 FROM model_routes WHERE deployment_id = duplicate_deployment_id) THEN
                RAISE EXCEPTION 'duplicate mock-chat deployment % is still referenced', duplicate_deployment_id;
            END IF;
            DELETE FROM upstream_model_deployments WHERE id = duplicate_deployment_id;
        END IF;

        UPDATE upstream_model_deployments
           SET upstream_model_id = 'mock-chat',
               internal_name = 'E2E NEW API Primary / mock-chat',
               source = 'manual',
               updated_at = now()
         WHERE id = current_deployment_id;
    END IF;

    SELECT id INTO STRICT canonical_deployment_id
      FROM upstream_model_deployments
     WHERE channel_id = primary_channel_id
       AND upstream_model_id = 'mock-chat'
       AND variant = '';

    INSERT INTO upstream_model_deployments
        (channel_id, upstream_model_id, internal_name, variant, status, source,
         procurement_price, created_at, updated_at)
    SELECT primary_channel_id, 'mock-error-500',
           'E2E NEW API Primary / mock-error-500', 'fault-injection',
           'disabled', 'test', procurement_price, now(), now()
      FROM upstream_model_deployments
     WHERE id = canonical_deployment_id
    ON CONFLICT (channel_id, upstream_model_id, variant)
    DO UPDATE SET procurement_price = EXCLUDED.procurement_price,
                  updated_at = now()
    RETURNING id INTO fault_deployment_id;

    IF fault_deployment_id IS NULL THEN
        SELECT id INTO STRICT fault_deployment_id
          FROM upstream_model_deployments
         WHERE channel_id = primary_channel_id
           AND upstream_model_id = 'mock-error-500'
           AND variant = 'fault-injection';
    END IF;

    IF current_setting('conduit.route_mode') = 'normal' THEN
        UPDATE model_routes
           SET deployment_id = canonical_deployment_id, updated_at = now()
         WHERE id = primary_route_id;
        UPDATE upstream_model_deployments
           SET status = CASE WHEN id = fault_deployment_id THEN 'disabled' ELSE status END,
               updated_at = CASE WHEN id = fault_deployment_id THEN now() ELSE updated_at END
         WHERE id = fault_deployment_id;
        UPDATE channels
           SET ordering_weight = 100, updated_at = now()
         WHERE id IN (primary_channel_id, secondary_channel_id);
    ELSE
        UPDATE upstream_model_deployments
           SET status = 'enabled', updated_at = now()
         WHERE id = fault_deployment_id;
        UPDATE model_routes
           SET deployment_id = fault_deployment_id, updated_at = now()
         WHERE id = primary_route_id;
        UPDATE channels
           SET ordering_weight = CASE
               WHEN id = primary_channel_id THEN 100
               WHEN id = secondary_channel_id THEN 1
           END,
               updated_at = now()
         WHERE id IN (primary_channel_id, secondary_channel_id);
    END IF;
END
$block$;

COMMIT;

SELECT c.name, c.ordering_weight, d.upstream_model_id, d.variant, d.status
  FROM models m
  JOIN model_routes mr ON mr.public_model_id = m.id
  JOIN upstream_model_deployments d ON d.id = mr.deployment_id
  JOIN channels c ON c.id = d.channel_id
 WHERE m.model_id = 'mock-chat'
 ORDER BY c.name;
'@

$sql = $common.Replace("__ROUTE_MODE__", $Mode)
& psql $DatabaseUrl -X -v ON_ERROR_STOP=1 -P pager=off -c $sql
if ($LASTEXITCODE -ne 0) {
    throw "Failed to switch mock route mode to $Mode"
}
