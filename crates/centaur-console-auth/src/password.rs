//! Password hashing (bcrypt, compatible with existing `password_digest`
//! values). Hashing and verification are CPU-heavy, so they run on the
//! blocking pool with bounded concurrency instead of stalling request
//! workers.

use std::sync::LazyLock;

use tokio::sync::Semaphore;

/// Cost for new hashes.
pub const COST: u32 = 12;

static PERMITS: LazyLock<Semaphore> = LazyLock::new(|| {
    Semaphore::new(
        std::thread::available_parallelism()
            .map_or(2, std::num::NonZeroUsize::get)
            .clamp(2, 8),
    )
});

/// A valid digest nobody has the password for, verified when the account
/// does not exist so a missing account takes as long as a wrong password.
static DUMMY_DIGEST: LazyLock<String> = LazyLock::new(|| {
    bcrypt::hash(crate::random::urlsafe_base64_32(), COST).expect("bcrypt hashing")
});

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let _permit = PERMITS.acquire().await.expect("semaphore is never closed");
    tokio::task::spawn_blocking(work)
        .await
        .expect("bcrypt task does not panic")
}

/// `$2a$` hash of `password` at `cost`.
pub async fn hash(password: &str, cost: u32) -> String {
    let password = password.to_owned();
    blocking(move || {
        bcrypt::hash_with_result(password, cost)
            .expect("bcrypt hashing")
            .format_for_version(bcrypt::Version::TwoA)
    })
    .await
}

/// Whether `password` matches `digest`. A missing or malformed digest never
/// matches, but still costs one verification.
pub async fn verify(password: &str, digest: Option<&str>) -> bool {
    let password = password.to_owned();
    let digest = digest
        .filter(|digest| !digest.is_empty())
        .map(str::to_owned);
    blocking(move || match digest {
        Some(digest) => bcrypt::verify(password, &digest).unwrap_or(false),
        None => {
            let _ = bcrypt::verify(password, &DUMMY_DIGEST);
            false
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn verifies_without_blocking_the_runtime() {
        let digest = super::hash("correct horse battery", 4).await;
        assert!(digest.starts_with("$2a$04$"));
        assert!(super::verify("correct horse battery", Some(&digest)).await);
        assert!(!super::verify("wrong", Some(&digest)).await);
        assert!(!super::verify("anything", None).await);
        assert!(!super::verify("anything", Some("not a digest")).await);
        // A single-threaded runtime keeps ticking while bcrypt runs.
        let tick = tokio::time::sleep(std::time::Duration::from_millis(5));
        let slow = super::hash("x", 10);
        tokio::pin!(tick, slow);
        tokio::select! {
            () = &mut tick => {}
            _ = &mut slow => panic!("hash finished before the timer"),
        }
    }
}
