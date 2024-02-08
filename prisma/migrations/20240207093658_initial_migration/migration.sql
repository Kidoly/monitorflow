-- CreateTable
CREATE TABLE `Metrics` (
    `id` INTEGER NOT NULL AUTO_INCREMENT,
    `date` DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    `startTime` DATETIME(3) NOT NULL,
    `totalMemory` BIGINT NULL,
    `usedMemory` BIGINT NOT NULL,
    `totalSwap` BIGINT NOT NULL,
    `usedSwap` BIGINT NOT NULL,
    `systemName` VARCHAR(191) NOT NULL,
    `kernelVersion` VARCHAR(191) NOT NULL,
    `osVersion` VARCHAR(191) NOT NULL,
    `hostName` VARCHAR(191) NOT NULL,
    `cpuCount` INTEGER NOT NULL,
    `cpuName` VARCHAR(191) NOT NULL,
    `disksNumbers` INTEGER NOT NULL,
    `disks` VARCHAR(191) NOT NULL,
    `networks` VARCHAR(191) NOT NULL,
    `components` VARCHAR(191) NOT NULL,
    `processesCount` INTEGER NOT NULL,
    `processes` LONGBLOB NULL,
    `monitor` LONGBLOB NULL,

    PRIMARY KEY (`id`)
) DEFAULT CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;

-- CreateTable
CREATE TABLE `User` (
    `id` INTEGER NOT NULL AUTO_INCREMENT,
    `email` VARCHAR(191) NOT NULL,
    `password` VARCHAR(191) NOT NULL,

    UNIQUE INDEX `User_email_key`(`email`),
    PRIMARY KEY (`id`)
) DEFAULT CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
