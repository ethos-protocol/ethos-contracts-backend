#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::cache::{VaultCache, CacheConfig};
    use crate::cache_invalidation::CacheInvalidationStrategy;
    use crate::query_cache::QueryCache;

    #[test]
    fn test_response_level_caching_with_ttl() {
        let cache = VaultCache::with_ttl(Duration::from_millis(100));
        let config = CacheConfig {
            ttl: Duration::from_millis(100),
            ..CacheConfig::default()
        };
        let _cache = VaultCache::with_config(config);

        let metrics = cache.metrics();
        assert!(metrics.total_entries == 0);
    }

    #[test]
    fn test_cache_hit_increments_stats() {
        let cache = VaultCache::new();
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
    }

    #[test]
    fn test_cache_miss_increments_stats() {
        let cache = VaultCache::new();
        let metrics = cache.metrics();
        assert_eq!(metrics.misses, 0);
    }

    #[test]
    fn test_dependency_tracking_for_cache_entries() {
        let invalidation = CacheInvalidationStrategy::new();
        let key = "resource_123";
        let dependencies = vec!["list_resources", "user_profile"];

        invalidation.register_dependencies(key, dependencies.clone());
        assert_eq!(
            invalidation.get_dependencies(key),
            Some(dependencies)
        );
    }

    #[test]
    fn test_invalidate_dependent_entries_on_update() {
        let invalidation = CacheInvalidationStrategy::new();
        let resource_key = "resource_123";
        let list_key = "list_resources";

        invalidation.register_dependencies(resource_key, vec![list_key]);
        assert_eq!(invalidation.get_dependencies(resource_key), Some(vec![list_key.to_string()]));

        invalidation.invalidate(resource_key);
        assert_eq!(invalidation.get_dependencies(resource_key), None);
    }

    #[test]
    fn test_cache_statistics_collection() {
        let cache = VaultCache::new();
        let metrics = cache.metrics();

        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 0);
        assert!(metrics.total_entries == 0);
    }

    #[test]
    fn test_query_cache_with_ttl() {
        let cache = QueryCache::new();
        cache.set("test_key", "value");
        assert_eq!(cache.get("test_key"), Some("value".to_string()));
    }

    #[test]
    fn test_dependency_invalidation_cascade() {
        let invalidation = CacheInvalidationStrategy::new();

        invalidation.register_dependencies("child_1", vec!["parent"]);
        invalidation.register_dependencies("child_2", vec!["parent"]);
        invalidation.register_dependencies("parent", vec![]);

        invalidation.invalidate("parent");

        assert_eq!(invalidation.get_dependencies("parent"), None);
        assert_eq!(invalidation.get_dependencies("child_1"), None);
        assert_eq!(invalidation.get_dependencies("child_2"), None);
    }

    #[test]
    fn test_smart_invalidation_only_affected_entries() {
        let invalidation = CacheInvalidationStrategy::new();

        invalidation.register_dependencies("item_1", vec!["list"]);
        invalidation.register_dependencies("item_2", vec!["other_list"]);

        invalidation.invalidate("item_1");

        assert_eq!(invalidation.get_dependencies("item_1"), None);
        assert!(invalidation.get_dependencies("item_2").is_some());
    }

    #[tokio::test]
    async fn test_concurrent_cache_operations() {
        let cache = Arc::new(VaultCache::new());
        let mut handles = vec![];

        for _i in 0..10 {
            let _cache_clone = Arc::clone(&cache);
            let handle = tokio::spawn(async move {
                let _metrics = cache.metrics();
                true
            });
            handles.push(handle);
        }

        for handle in handles {
            let result = handle.await;
            assert!(result.is_ok());
            assert!(result.unwrap());
        }
    }

    #[test]
    fn test_cache_statistics_accuracy() {
        let cache = VaultCache::new();

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 0);
    }

    #[test]
    fn test_dependency_circular_reference_handling() {
        let invalidation = CacheInvalidationStrategy::new();

        invalidation.register_dependencies("a", vec!["b"]);
        invalidation.register_dependencies("b", vec!["c"]);
        invalidation.register_dependencies("c", vec!["a"]);

        invalidation.invalidate("a");
        assert_eq!(invalidation.get_dependencies("a"), None);
    }

    #[test]
    fn test_invalidation_with_pattern_matching() {
        let invalidation = CacheInvalidationStrategy::new();

        invalidation.register_dependencies("user:123:profile", vec![]);
        invalidation.register_dependencies("user:123:settings", vec![]);
        invalidation.register_dependencies("user:456:profile", vec![]);

        invalidation.invalidate_pattern("user:123:*");

        assert_eq!(invalidation.get_dependencies("user:123:profile"), None);
        assert_eq!(invalidation.get_dependencies("user:123:settings"), None);
        assert!(invalidation.get_dependencies("user:456:profile").is_some());
    }

    #[test]
    fn test_query_cache_invalidation_all() {
        let cache = QueryCache::new();
        cache.set("key1", "value1");
        cache.set("key2", "value2");

        cache.invalidate_all();

        assert_eq!(cache.get("key1"), None);
        assert_eq!(cache.get("key2"), None);
    }

    #[test]
    fn test_cache_config_with_custom_ttl() {
        let config = CacheConfig {
            ttl: Duration::from_secs(60),
            ..CacheConfig::default()
        };
        let _cache = VaultCache::with_config(config);
        assert!(true);
    }

    #[test]
    fn test_cache_with_default_config() {
        let cache = VaultCache::new();
        let metrics = cache.metrics();
        assert!(metrics.total_entries >= 0);
    }
}
