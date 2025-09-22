package __package__.domain.convert.sku;

import __package__.common.convert.EntityConverter;
import __package__.domain.model.sku.SkuDO;
import __package__.domain.entity.sku.SkuExampleEntity;
import org.mapstruct.Mapper;

/**
 * SkuExampleEntityConvert
 *
 * @author weirenyan
 * @date 2024/5/9
 */
@Mapper(componentModel = "spring", uses = {})
public interface SkuExampleEntityConvert extends EntityConverter<SkuExampleEntity, SkuDO> {
}
