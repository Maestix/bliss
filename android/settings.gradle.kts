pluginManagement {
    repositories {
        google()
        // Google's mirror of Maven Central first: Central itself answers a
        // burst of parallel requests with 429.
        maven("https://maven-central.storage-download.googleapis.com/maven2/")
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        // Google's mirror of Maven Central first: Central itself answers a
        // burst of parallel requests with 429.
        maven("https://maven-central.storage-download.googleapis.com/maven2/")
        mavenCentral()
    }
}

rootProject.name = "bliss"
include(":app")
